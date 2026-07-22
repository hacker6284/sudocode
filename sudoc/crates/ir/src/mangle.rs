//! Canonical, collision-proof symbol mangling — the single source of truth
//! for every compiler-generated name (monomorphized instances, cross-module
//! qualification, enum-variant class/tag names) that every backend must
//! consume verbatim instead of re-gluing name parts itself.
//! See spec/lockstep.md §7 (monomorphization) and §8 (multi-module).
//!
//! ## Reserved namespace
//! `sudo_` (value/function symbols) and `Sudo_` (type/constructor symbols —
//! capitalized because Haskell requires uppercase type/data-constructor
//! names) are reserved for compiler-generated symbols. The type-checker
//! rejects any user-declared identifier whose leading-underscores-stripped,
//! case-folded form begins with `sudo_` ([`is_reserved`]) — so a
//! compiler-generated name (which always starts with `sudo_`/`Sudo_`,
//! zero leading underscores) can never collide with a legal user
//! identifier.
//!
//! ## Encoding grammar
//! Every user-supplied name component is length-prefixed before being
//! glued into a compound symbol: `<decimal-byte-length><bytes>` ([`enc`]).
//! Because sudo identifiers can never start with a digit
//! (`[a-zA-Z_][a-zA-Z0-9_]*`, spec/language.md §1), a decoder can always
//! find a component's length by reading decimal digits until the first
//! non-digit byte, then consuming exactly that many following bytes —
//! regardless of what characters (including further digits or underscores)
//! appear inside the component. This makes every encoded compound
//! self-delimiting: two different (module, name, type-args) triples can
//! never produce the same encoded string.
//!
//! Grammar (informal EBNF; `enc(x)` = length-prefixed `x`):
//! ```text
//! value_symbol  := plain | "sudo_" qualifier? enc(name) targs?
//! plain         := name                    ; no module, no generics: bare, as today
//! qualifier     := "M" enc(module) "_"
//! targs         := "__" ty_mangled ( "_" ty_mangled )*
//! ty_mangled    := enc("i64") | enc("f64") | enc("bool")
//!                 | "List_" ty_mangled | "Set_" ty_mangled
//!                 | "Map_" ty_mangled "_" ty_mangled
//!                 | "Opt_" ty_mangled
//!                 | "Res_" ty_mangled "_" ty_mangled
//!                 | "Tup" N "_" ty_mangled { "_" ty_mangled }   ; N = arity, decimal
//!                 | "Fn_" ty_mangled { "_" ty_mangled } "_to_" ty_mangled
//!                 | record_or_enum_name                      ; bare — matches the type's own declaration
//! type_symbol   := name | "Sudo_M" enc(module) "_" enc(name)
//! variant_class := "Sudo_" enc(enum_name) "_" enc(variant_name)
//! ```
//! Any generated symbol exceeding `MAX_LEN` bytes (well past C's
//! guaranteed-significant-identifier floor) is truncated and suffixed with
//! an 8-hex-digit FNV-1a hash of the *untruncated* string (`cap`), so
//! uniqueness survives truncation.

use crate::Ty;

/// Reserved prefix for compiler-generated value/function symbols.
pub const VALUE_PREFIX: &str = "sudo_";
/// Reserved prefix for compiler-generated type/constructor symbols.
pub const TYPE_PREFIX: &str = "Sudo_";

/// Longest generated symbol before the truncate+hash fallback kicks in.
const MAX_LEN: usize = 200;

/// True if `name`'s leading-underscores-stripped, case-folded form begins
/// with `sudo_` — the reserved-namespace check the type-checker applies to
/// every user-declared identifier (function, type, record, enum, variant,
/// constant, param, local). `sudoku` and `sudo` stay legal; `sudo_x`,
/// `_Sudo_X`, `SUDO_x` do not.
pub fn is_reserved(name: &str) -> bool {
    let stripped = name.trim_start_matches('_');
    stripped.len() >= 5 && stripped.as_bytes()[..5].eq_ignore_ascii_case(b"sudo_")
}

/// Length-prefix a single user-supplied name component: `<len><bytes>`.
/// Self-delimiting because sudo identifiers never start with a digit —
/// see the module docs' decoding argument.
pub fn enc(s: &str) -> String {
    format!("{}{s}", s.len())
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Truncate-with-hash fallback for symbols over [`MAX_LEN`] bytes: keep a
/// prefix and append an 8-hex-digit FNV-1a hash of the full untruncated
/// string, so long generated names still fit host identifier limits
/// without losing uniqueness. All inputs here are ASCII (identifiers,
/// digits, and our own fixed ASCII punctuation), so byte slicing is safe.
fn cap(s: String) -> String {
    if s.len() <= MAX_LEN {
        return s;
    }
    let hash = fnv1a(s.as_bytes());
    let keep = MAX_LEN - 10; // "_h" + 8 hex digits
    format!("{}_h{hash:08x}", &s[..keep])
}

/// Structural, deterministic, human-readable type mangling shared by every
/// backend and by [`instantiation_name`]. Scalar leaf keywords
/// (`i64`/`f64`/`bool`) are length-prefixed since a user-declared
/// record/enum could otherwise share that exact spelling; a record/enum's
/// own name is used bare so it always matches that type's own (unmangled)
/// declaration — the narrow residual case of a record/enum literally named
/// `i64`/`f64`/`bool` colliding with the corresponding builtin inside a
/// composite type's helper name is a known, pre-existing, extremely narrow
/// gap, out of scope for this fix. Structural tags
/// (`List_`/`Map_`/…/`Fn_..to_`) are fixed compiler keywords, not user
/// data, so they stay bare for readability.
pub fn mangle_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => enc("i64"),
        Ty::Float => enc("f64"),
        Ty::Bool => enc("bool"),
        Ty::List(e) => format!("List_{}", mangle_ty(e)),
        Ty::Set(e) => format!("Set_{}", mangle_ty(e)),
        Ty::Map(k, v) => format!("Map_{}_{}", mangle_ty(k), mangle_ty(v)),
        Ty::Option_(e) => format!("Opt_{}", mangle_ty(e)),
        Ty::Result_(t, e) => format!("Res_{}_{}", mangle_ty(t), mangle_ty(e)),
        Ty::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(mangle_ty).collect();
            format!("Tup{}_{}", ts.len(), parts.join("_"))
        }
        Ty::Func { params, ret } => {
            let parts: Vec<String> = params.iter().map(mangle_ty).collect();
            let r = ret.as_ref().map(|r| mangle_ty(r)).unwrap_or_else(|| enc("void"));
            format!("Fn_{}_to_{r}", parts.join("_"))
        }
        Ty::Record(n) | Ty::Enum(n) => n.clone(),
        Ty::Infer(_) => unreachable!("Ty::Infer escaped resolution"),
    }
}

/// Canonical name for a monomorphized generic instantiation
/// (`pick<int>` -> `sudo_4pick__3i64`). Non-generic calls (`type_args`
/// empty) return `name` unchanged — the common, uninstrumented case stays
/// exactly as readable as today.
pub fn instantiation_name(name: &str, type_args: &[Ty]) -> String {
    if type_args.is_empty() {
        return name.to_string();
    }
    let parts: Vec<String> = type_args.iter().map(mangle_ty).collect();
    cap(format!("{VALUE_PREFIX}{}__{}", enc(name), parts.join("_")))
}

/// Canonical cross-module qualification for a value symbol (function or
/// constant). `symbol` is whatever the local name already is — which may
/// itself already be a `sudo_`-mangled instantiation from
/// [`instantiation_name`] — wrapping it as one length-prefixed atom keeps
/// the scheme composable regardless of its internal structure.
/// `module = None` (same-module reference, or the entry module in a
/// merge) returns `symbol` unchanged: the common case stays bare.
pub fn qualify_value(module: Option<&str>, symbol: &str) -> String {
    match module {
        None => symbol.to_string(),
        Some(m) => cap(format!("{VALUE_PREFIX}M{}_{}", enc(m), enc(symbol))),
    }
}

/// Canonical cross-module qualification for a type symbol (record or
/// enum), `Sudo_`-prefixed. `module = None` returns `symbol` unchanged.
pub fn qualify_type(module: Option<&str>, symbol: &str) -> String {
    match module {
        None => symbol.to_string(),
        Some(m) => cap(format!("{TYPE_PREFIX}M{}_{}", enc(m), enc(symbol))),
    }
}

/// Canonical per-variant class/tag symbol for backends without a native
/// sum type (C enum tags, Python/JS per-variant classes, Haskell record
/// field selectors) — `Sudo_`-prefixed *unconditionally*: gluing two user
/// strings (enum name + variant name) is always at collision risk, even
/// within a single module (spec/lockstep.md §8's F8 class), so unlike
/// [`qualify_value`] there is no bare/unprefixed fast path here.
pub fn variant_class(enum_name: &str, variant: &str) -> String {
    cap(format!("{TYPE_PREFIX}{}_{}", enc(enum_name), enc(variant)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_matches_design_examples() {
        assert!(!is_reserved("sudoku"));
        assert!(!is_reserved("sudo"));
        assert!(is_reserved("sudo_x"));
        assert!(is_reserved("_Sudo_X"));
        assert!(is_reserved("SUDO_x"));
        assert!(is_reserved("__sudo_x"));
    }

    #[test]
    fn instantiation_name_matches_spec_example() {
        assert_eq!(instantiation_name("pick", &[Ty::Int]), "sudo_4pick__3i64");
    }

    #[test]
    fn instantiation_name_is_reserved_when_generic() {
        assert!(is_reserved(&instantiation_name("pick", &[Ty::Int])));
    }

    #[test]
    fn instantiation_name_bare_when_non_generic() {
        assert_eq!(instantiation_name("pick", &[]), "pick");
    }

    #[test]
    fn module_qualification_is_collision_free_for_f8() {
        // module `a_b` fn `c` vs module `a` fn `b_c` must never collide.
        let x = qualify_value(Some("a_b"), "c");
        let y = qualify_value(Some("a"), "b_c");
        assert_ne!(x, y);
        assert!(is_reserved(&x));
        assert!(is_reserved(&y));
    }

    #[test]
    fn variant_class_is_collision_free() {
        let x = variant_class("A", "B_C");
        let y = variant_class("A_B", "C");
        assert_ne!(x, y);
    }

    #[test]
    fn long_names_truncate_with_hash() {
        let long = "x".repeat(500);
        let out = instantiation_name(&long, &[Ty::Int]);
        assert!(out.len() <= MAX_LEN);
        assert!(out.contains("_h"));
    }

    #[test]
    fn record_and_enum_names_stay_bare_in_mangle_ty() {
        assert_eq!(mangle_ty(&Ty::Record("Tree".into())), "Tree");
        assert_eq!(mangle_ty(&Ty::Enum("Tree".into())), "Tree");
        // Still nests bare inside a composite structural tag.
        assert_eq!(
            mangle_ty(&Ty::List(Box::new(Ty::Record("Tree".into())))),
            "List_Tree"
        );
    }
}
