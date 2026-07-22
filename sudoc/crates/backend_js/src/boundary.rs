//! Host-boundary adapters for JavaScript (lockstep.md §5.4): for each
//! adaptable export, a wrapper function in the host-facing API module that
//! converts host JS values in (validating/copying), calls the internal
//! `_impl` function (BigInt/internal representation), and converts results
//! back out (Option collapse, Result→throw, text↔string, plain-object
//! records, tagged-object enums), recursively for nested types.
//!
//! Adaptable surface (§5.4): every row of the boundary table composes
//! recursively, *except* function-typed values, which can never cross the
//! boundary (and are in fact already a hard compile error anywhere in a
//! top-level export signature — crates/types/src/lib.rs
//! `validate_export_boundary`). The one gap that check doesn't close: a
//! *record or enum field* may itself be function-typed without tripping
//! that top-level scan (it only walks List/Set/Map/Option/Result/Tuple, not
//! into named record/enum fields). This module's own `boundary_in_ok` /
//! `boundary_out_ok` close that gap by walking record/enum fields too, so a
//! record containing a `func` field is correctly declared internal-only
//! here rather than crashing codegen.
//!
//! `Result<T, E>` is asymmetric per the boundary table: it has a defined
//! *out* conversion (`Ok`→value, `Err`→throw `SudoError`) but no *in*
//! conversion — the table's "in" column is blank. So a `Result` appearing
//! anywhere in a parameter's type (top-level or nested inside a record/List/
//! etc reachable from a parameter) makes that export non-adaptable; a
//! `Result` in a return type (or an inout param's outgoing side) is fine.
//!
//! KNOWN GAP (flagged, not silently improvised around): record/enum field
//! types are stored in the IR as `Ty`, which — unlike `BoundaryTy` — has
//! already erased `text` to `List<int>` (see `sudoc_ir::Ty`'s doc comment).
//! A record field declared `text` therefore cannot be distinguished from a
//! `List<int>` field once it's nested inside a record/enum, and this
//! adapter necessarily renders it as an array of code-point numbers rather
//! than a JS string at that nested position. Top-level `text` params/return
//! values (the common case) are unaffected — only `text` *nested inside a
//! record or enum field* degrades this way. Fixing this precisely would
//! require carrying boundary-type information into record/enum field
//! declarations in the IR itself, out of this backend's scope.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use sudoc_ir::{BoundaryTy, IrFunc, IrModule, IrParam, Ty};

/// File name of the host-facing API module (only emitted when at least one
/// export has an adaptable signature).
pub fn api_file(module: &str) -> String {
    format!("{module}.mjs")
}

/// A type shape shared by `BoundaryTy` (export signatures) and record/enum
/// field `Ty` (nested boundary shape, `text` already erased — see the
/// module doc's KNOWN GAP note). Unifies the two so conversion codegen is
/// written once and reused at every nesting depth.
enum BTy {
    Int,
    Float,
    Bool,
    Text,
    List(Box<BTy>),
    Set(Box<BTy>),
    Map(Box<BTy>, Box<BTy>),
    Option_(Box<BTy>),
    Result_(Box<BTy>, Box<BTy>),
    Tuple(Vec<BTy>),
    Named(String),
}

fn bty_of_boundary(te: &BoundaryTy) -> BTy {
    match te {
        BoundaryTy::Int => BTy::Int,
        BoundaryTy::Float => BTy::Float,
        BoundaryTy::Bool => BTy::Bool,
        BoundaryTy::Text => BTy::Text,
        BoundaryTy::List(t) => BTy::List(Box::new(bty_of_boundary(t))),
        BoundaryTy::Set(t) => BTy::Set(Box::new(bty_of_boundary(t))),
        BoundaryTy::Map(k, v) => {
            BTy::Map(Box::new(bty_of_boundary(k)), Box::new(bty_of_boundary(v)))
        }
        BoundaryTy::Option_(t) => BTy::Option_(Box::new(bty_of_boundary(t))),
        BoundaryTy::Result_(t, e) => {
            BTy::Result_(Box::new(bty_of_boundary(t)), Box::new(bty_of_boundary(e)))
        }
        BoundaryTy::Tuple(ts) => BTy::Tuple(ts.iter().map(bty_of_boundary).collect()),
        BoundaryTy::Named(n) => BTy::Named(n.clone()),
        BoundaryTy::Func { .. } => unreachable!("non-adaptable boundary type reached codegen"),
    }
}

/// Map a record/enum field `Ty` into the shared boundary shape.
///
/// **KNOWN GAP:** `Ty` has already erased `text` to `List<int>`, so a field
/// declared `text` is indistinguishable from `List<int>` and surfaces at the
/// JS boundary as an array of code-point numbers rather than a string. Only
/// nested `text` (inside a record/enum field) degrades this way; top-level
/// `text` params/returns use `BoundaryTy::Text` via `bty_of_boundary` and are
/// unaffected. See the module doc.
fn bty_of_ty(ty: &Ty) -> BTy {
    match ty {
        Ty::Int => BTy::Int,
        Ty::Float => BTy::Float,
        Ty::Bool => BTy::Bool,
        Ty::List(t) => BTy::List(Box::new(bty_of_ty(t))),
        Ty::Set(t) => BTy::Set(Box::new(bty_of_ty(t))),
        Ty::Map(k, v) => BTy::Map(Box::new(bty_of_ty(k)), Box::new(bty_of_ty(v))),
        Ty::Option_(t) => BTy::Option_(Box::new(bty_of_ty(t))),
        Ty::Result_(t, e) => BTy::Result_(Box::new(bty_of_ty(t)), Box::new(bty_of_ty(e))),
        Ty::Tuple(ts) => BTy::Tuple(ts.iter().map(bty_of_ty).collect()),
        Ty::Record(n) | Ty::Enum(n) => BTy::Named(n.clone()),
        Ty::Func { .. } | Ty::Infer(_) => unreachable!("non-adaptable ty reached codegen"),
    }
}

// ---- adaptability (direction-aware: Result has no "in" mapping) -----------

fn boundary_in_ok(te: &BoundaryTy, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    match te {
        BoundaryTy::Func { .. } | BoundaryTy::Result_(..) => false,
        BoundaryTy::Int | BoundaryTy::Float | BoundaryTy::Bool | BoundaryTy::Text => true,
        BoundaryTy::List(t) | BoundaryTy::Set(t) | BoundaryTy::Option_(t) => {
            boundary_in_ok(t, m, visiting)
        }
        BoundaryTy::Map(k, v) => boundary_in_ok(k, m, visiting) && boundary_in_ok(v, m, visiting),
        BoundaryTy::Tuple(ts) => ts.iter().all(|t| boundary_in_ok(t, m, visiting)),
        BoundaryTy::Named(n) => named_in_ok(n, m, visiting),
    }
}

fn boundary_out_ok(te: &BoundaryTy, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    match te {
        BoundaryTy::Func { .. } => false,
        BoundaryTy::Int | BoundaryTy::Float | BoundaryTy::Bool | BoundaryTy::Text => true,
        BoundaryTy::List(t) | BoundaryTy::Set(t) | BoundaryTy::Option_(t) => {
            boundary_out_ok(t, m, visiting)
        }
        BoundaryTy::Map(k, v) => boundary_out_ok(k, m, visiting) && boundary_out_ok(v, m, visiting),
        BoundaryTy::Result_(t, e) => {
            boundary_out_ok(t, m, visiting) && boundary_out_ok(e, m, visiting)
        }
        BoundaryTy::Tuple(ts) => ts.iter().all(|t| boundary_out_ok(t, m, visiting)),
        BoundaryTy::Named(n) => named_out_ok(n, m, visiting),
    }
}

fn named_in_ok(name: &str, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    if !visiting.insert(name.to_string()) {
        return true; // cycle: nothing new found yet, no proof of non-adaptability
    }
    let ok = if let Some(r) = m.record(name) {
        r.fields.iter().all(|(_, ty)| ty_in_ok(ty, m, visiting))
    } else if let Some(e) = m.enum_(name) {
        e.variants
            .iter()
            .all(|v| v.fields.iter().all(|(_, ty)| ty_in_ok(ty, m, visiting)))
    } else {
        false
    };
    visiting.remove(name);
    ok
}

fn named_out_ok(name: &str, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    if !visiting.insert(name.to_string()) {
        return true;
    }
    let ok = if let Some(r) = m.record(name) {
        r.fields.iter().all(|(_, ty)| ty_out_ok(ty, m, visiting))
    } else if let Some(e) = m.enum_(name) {
        e.variants
            .iter()
            .all(|v| v.fields.iter().all(|(_, ty)| ty_out_ok(ty, m, visiting)))
    } else {
        false
    };
    visiting.remove(name);
    ok
}

fn ty_in_ok(ty: &Ty, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    match ty {
        Ty::Func { .. } | Ty::Result_(..) | Ty::Infer(_) => false,
        Ty::Int | Ty::Float | Ty::Bool => true,
        Ty::List(t) | Ty::Set(t) | Ty::Option_(t) => ty_in_ok(t, m, visiting),
        Ty::Map(k, v) => ty_in_ok(k, m, visiting) && ty_in_ok(v, m, visiting),
        Ty::Tuple(ts) => ts.iter().all(|t| ty_in_ok(t, m, visiting)),
        Ty::Record(n) | Ty::Enum(n) => named_in_ok(n, m, visiting),
    }
}

fn ty_out_ok(ty: &Ty, m: &IrModule, visiting: &mut HashSet<String>) -> bool {
    match ty {
        Ty::Func { .. } | Ty::Infer(_) => false,
        Ty::Int | Ty::Float | Ty::Bool => true,
        Ty::List(t) | Ty::Set(t) | Ty::Option_(t) => ty_out_ok(t, m, visiting),
        Ty::Map(k, v) => ty_out_ok(k, m, visiting) && ty_out_ok(v, m, visiting),
        Ty::Result_(t, e) => ty_out_ok(t, m, visiting) && ty_out_ok(e, m, visiting),
        Ty::Tuple(ts) => ts.iter().all(|t| ty_out_ok(t, m, visiting)),
        Ty::Record(n) | Ty::Enum(n) => named_out_ok(n, m, visiting),
    }
}

fn param_ok(p: &IrParam, m: &IrModule) -> bool {
    let mut v = HashSet::new();
    if !boundary_in_ok(&p.boundary, m, &mut v) {
        return false;
    }
    if p.inout {
        let mut v2 = HashSet::new();
        if !boundary_out_ok(&p.boundary, m, &mut v2) {
            return false;
        }
    }
    true
}

fn func_adaptable(f: &IrFunc, m: &IrModule) -> bool {
    let params_ok = f.params.iter().all(|p| param_ok(p, m));
    let ret_ok = f
        .ret_boundary
        .as_ref()
        .map(|rb| {
            let mut v = HashSet::new();
            boundary_out_ok(rb, m, &mut v)
        })
        .unwrap_or(true);
    params_ok && ret_ok
}

// ---- transitive named-type collection (for helper generation) -------------

fn collect_named_boundary(te: &BoundaryTy, m: &IrModule, out: &mut BTreeSet<String>) {
    match te {
        BoundaryTy::List(t) | BoundaryTy::Set(t) | BoundaryTy::Option_(t) => {
            collect_named_boundary(t, m, out);
        }
        BoundaryTy::Map(k, v) => {
            collect_named_boundary(k, m, out);
            collect_named_boundary(v, m, out);
        }
        BoundaryTy::Result_(t, e) => {
            collect_named_boundary(t, m, out);
            collect_named_boundary(e, m, out);
        }
        BoundaryTy::Tuple(ts) => {
            for t in ts {
                collect_named_boundary(t, m, out);
            }
        }
        BoundaryTy::Named(n) => collect_named_transitive(n, m, out),
        BoundaryTy::Int
        | BoundaryTy::Float
        | BoundaryTy::Bool
        | BoundaryTy::Text
        | BoundaryTy::Func { .. } => {}
    }
}

fn collect_named_ty(ty: &Ty, m: &IrModule, out: &mut BTreeSet<String>) {
    match ty {
        Ty::List(t) | Ty::Set(t) | Ty::Option_(t) => collect_named_ty(t, m, out),
        Ty::Map(k, v) => {
            collect_named_ty(k, m, out);
            collect_named_ty(v, m, out);
        }
        Ty::Result_(t, e) => {
            collect_named_ty(t, m, out);
            collect_named_ty(e, m, out);
        }
        Ty::Tuple(ts) => {
            for t in ts {
                collect_named_ty(t, m, out);
            }
        }
        Ty::Record(n) | Ty::Enum(n) => collect_named_transitive(n, m, out),
        Ty::Int | Ty::Float | Ty::Bool | Ty::Func { .. } | Ty::Infer(_) => {}
    }
}

fn collect_named_transitive(name: &str, m: &IrModule, out: &mut BTreeSet<String>) {
    if !out.insert(name.to_string()) {
        return; // already visited (or being visited) — cycle-safe
    }
    if let Some(r) = m.record(name) {
        for (_, ty) in &r.fields {
            collect_named_ty(ty, m, out);
        }
    } else if let Some(e) = m.enum_(name) {
        for v in &e.variants {
            for (_, ty) in &v.fields {
                collect_named_ty(ty, m, out);
            }
        }
    }
}

// ---- conversion expression codegen -----------------------------------------

fn conv_in(bty: &BTy, var: &str) -> String {
    match bty {
        BTy::Int => format!("_rt.host_int({var})"),
        BTy::Float => format!("_rt.host_float({var})"),
        BTy::Bool => format!("_rt.host_bool({var})"),
        BTy::Text => format!("_rt.host_text({var})"),
        BTy::List(t) => format!("_rt.host_list({var}, (_v) => {})", conv_in(t, "_v")),
        BTy::Set(t) => format!("_rt.host_set({var}, (_v) => {})", conv_in(t, "_v")),
        BTy::Map(k, v) => format!(
            "_rt.host_map({var}, (_k) => {}, (_v) => {})",
            conv_in(k, "_k"),
            conv_in(v, "_v")
        ),
        BTy::Option_(t) => format!(
            "(({var}) === null || ({var}) === undefined ? _rt.NONE : new _rt.Some({}))",
            conv_in(t, var)
        ),
        BTy::Tuple(ts) => {
            let convs: Vec<String> =
                ts.iter().map(|t| format!("(_v) => {}", conv_in(t, "_v"))).collect();
            format!("_rt.host_tuple({var}, {}, [{}])", ts.len(), convs.join(", "))
        }
        BTy::Named(name) => format!("_sudo_conv_in_{name}({var})"),
        BTy::Result_(..) => {
            unreachable!("Result has no boundary 'in' mapping — caught by boundary_in_ok")
        }
    }
}

fn conv_out(bty: &BTy, var: &str) -> String {
    match bty {
        BTy::Int => format!("_rt.int_out({var})"),
        BTy::Float | BTy::Bool => var.to_string(),
        BTy::Text => format!("_rt.text_str({var})"),
        BTy::List(t) => format!("{var}.map((_v) => {})", conv_out(t, "_v")),
        BTy::Set(t) => format!("_rt.out_set({var}, (_v) => {})", conv_out(t, "_v")),
        BTy::Map(k, v) => format!(
            "_rt.out_map({var}, (_k) => {}, (_v) => {})",
            conv_out(k, "_k"),
            conv_out(v, "_v")
        ),
        BTy::Option_(t) => format!("_rt.out_option({var}, (_v) => {})", conv_out(t, "_v")),
        BTy::Result_(t, e) => format!(
            "_rt.out_result({var}, (_v) => {}, (_e) => {})",
            conv_out(t, "_v"),
            conv_out(e, "_e")
        ),
        BTy::Tuple(ts) => {
            let items: Vec<String> = ts
                .iter()
                .enumerate()
                .map(|(i, t)| conv_out(t, &format!("{var}[{i}]")))
                .collect();
            format!("[{}]", items.join(", "))
        }
        BTy::Named(name) => format!("_sudo_conv_out_{name}({var})"),
    }
}

// ---- record/enum helper function emission ----------------------------------

fn emit_named_in_helper(name: &str, m: &IrModule, out: &mut String) {
    if let Some(r) = m.record(name) {
        let args: Vec<String> = r
            .fields
            .iter()
            .map(|(fname, fty)| conv_in(&bty_of_ty(fty), &format!("_v.{fname}")))
            .collect();
        let _ = writeln!(out, "function _sudo_conv_in_{name}(_v) {{");
        let _ = writeln!(
            out,
            "    if (!(_v && typeof _v === \"object\")) throw new TypeError(\"expected a plain object for {name}\");"
        );
        let _ = writeln!(out, "    return new _impl.{name}({});", args.join(", "));
        let _ = writeln!(out, "}}");
        let _ = writeln!(out);
    } else if let Some(e) = m.enum_(name) {
        let _ = writeln!(out, "function _sudo_conv_in_{name}(_v) {{");
        let _ = writeln!(
            out,
            "    if (!(_v && typeof _v === \"object\") || typeof _v.$ !== \"string\") throw new TypeError(\"expected a tagged object for {name}\");"
        );
        let _ = writeln!(out, "    switch (_v.$) {{");
        for v in &e.variants {
            let args: Vec<String> = v
                .fields
                .iter()
                .map(|(fname, fty)| conv_in(&bty_of_ty(fty), &format!("_v.{fname}")))
                .collect();
            let _ = writeln!(out, "        case \"{}\":", v.name);
            let _ = writeln!(
                out,
                "            return new _impl.{}({});",
                sudoc_ir::mangle::variant_class(name, &v.name),
                args.join(", ")
            );
        }
        let _ = writeln!(
            out,
            "        default: throw new TypeError(`unknown variant \"${{_v.$}}\" for {name}`);"
        );
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "}}");
        let _ = writeln!(out);
    }
}

fn emit_named_out_helper(name: &str, m: &IrModule, out: &mut String) {
    if let Some(r) = m.record(name) {
        let fields: Vec<String> = r
            .fields
            .iter()
            .map(|(fname, fty)| {
                format!("{fname}: {}", conv_out(&bty_of_ty(fty), &format!("_v.{fname}")))
            })
            .collect();
        let _ = writeln!(out, "function _sudo_conv_out_{name}(_v) {{");
        let _ = writeln!(out, "    return {{ {} }};", fields.join(", "));
        let _ = writeln!(out, "}}");
        let _ = writeln!(out);
    } else if let Some(e) = m.enum_(name) {
        let _ = writeln!(out, "function _sudo_conv_out_{name}(_v) {{");
        for v in &e.variants {
            let cls = sudoc_ir::mangle::variant_class(name, &v.name);
            let mut fields = vec![format!("\"$\": \"{}\"", v.name)];
            for (fname, fty) in &v.fields {
                fields.push(format!(
                    "{fname}: {}",
                    conv_out(&bty_of_ty(fty), &format!("_v.{fname}"))
                ));
            }
            let _ = writeln!(
                out,
                "    if (_v instanceof _impl.{cls}) return {{ {} }};",
                fields.join(", ")
            );
        }
        let _ = writeln!(out, "    throw new TypeError(\"unknown {name} variant instance\");");
        let _ = writeln!(out, "}}");
        let _ = writeln!(out);
    }
}

// ---- export wrappers ---------------------------------------------------------

fn emit_wrapper(f: &IrFunc, out: &mut String) {
    let params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let _ = writeln!(out, "export function {}({}) {{", f.name, params.join(", "));
    for p in &f.params {
        let bty = bty_of_boundary(&p.boundary);
        let converted = conv_in(&bty, &p.name);
        if p.inout {
            let _ = writeln!(out, "    const _in_{} = {converted};", p.name);
        } else {
            let _ = writeln!(out, "    {} = {converted};", p.name);
        }
    }
    let call_args: Vec<String> = f
        .params
        .iter()
        .map(|p| if p.inout { format!("_in_{}", p.name) } else { p.name.clone() })
        .collect();
    let mut targets: Vec<String> = Vec::new();
    if f.ret.is_some() {
        targets.push("_r".into());
    }
    for p in f.params.iter().filter(|p| p.inout) {
        targets.push(format!("_new_{}", p.name));
    }
    let call = format!("_impl.{}({})", f.name, call_args.join(", "));
    match targets.len() {
        0 => {
            let _ = writeln!(out, "    {call};");
        }
        1 => {
            let _ = writeln!(out, "    const {} = {call};", targets[0]);
        }
        _ => {
            let _ = writeln!(out, "    const [{}] = {call};", targets.join(", "));
        }
    }
    for p in f.params.iter().filter(|p| p.inout) {
        let new = format!("_new_{}", p.name);
        match &p.boundary {
            BoundaryTy::List(t) => {
                let bty = bty_of_boundary(t);
                let _ = writeln!(
                    out,
                    "    _rt.writeback_list({}, {new}, (_v) => {});",
                    p.name,
                    conv_out(&bty, "_v")
                );
            }
            BoundaryTy::Map(k, v) => {
                let bk = bty_of_boundary(k);
                let bv = bty_of_boundary(v);
                let _ = writeln!(
                    out,
                    "    _rt.writeback_map({}, {new}, (_k) => {}, (_v) => {});",
                    p.name,
                    conv_out(&bk, "_k"),
                    conv_out(&bv, "_v")
                );
            }
            BoundaryTy::Set(t) => {
                let bty = bty_of_boundary(t);
                let _ = writeln!(
                    out,
                    "    _rt.writeback_set({}, {new}, (_v) => {});",
                    p.name,
                    conv_out(&bty, "_v")
                );
            }
            BoundaryTy::Named(name) => {
                let _ = writeln!(
                    out,
                    "    Object.assign({}, _sudo_conv_out_{name}({new}));",
                    p.name
                );
            }
            _ => unreachable!("checker restricts inout export params to List/Map/Set/Record"),
        }
    }
    if let Some(rb) = &f.ret_boundary {
        let bty = bty_of_boundary(rb);
        let _ = writeln!(out, "    return {};", conv_out(&bty, "_r"));
    } else if f.ret.is_some() && targets.iter().any(|t| t == "_r") {
        let _ = writeln!(out, "    return _r;");
    }
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
}

/// The host-facing adapter module, or `None` when no export has an
/// adaptable signature (mirrors the C backend's convention: an entirely
/// non-adaptable export set publishes nothing; the host falls back to the
/// internal `_impl` module directly, which already exports every function
/// at the JS-module level).
pub fn emit_api(m: &IrModule) -> Option<String> {
    let exports: Vec<&IrFunc> = m.funcs.iter().filter(|f| f.export).collect();
    if exports.is_empty() {
        return None;
    }
    let adapted: Vec<&IrFunc> = exports.iter().filter(|f| func_adaptable(f, m)).copied().collect();
    if adapted.is_empty() {
        return None;
    }
    let skipped: Vec<&IrFunc> = exports.iter().filter(|f| !func_adaptable(f, m)).copied().collect();

    let mut needed_in: BTreeSet<String> = BTreeSet::new();
    let mut needed_out: BTreeSet<String> = BTreeSet::new();
    for f in &adapted {
        for p in &f.params {
            collect_named_boundary(&p.boundary, m, &mut needed_in);
            if p.inout {
                collect_named_boundary(&p.boundary, m, &mut needed_out);
            }
        }
        if let Some(rb) = &f.ret_boundary {
            collect_named_boundary(rb, m, &mut needed_out);
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Host-facing API for {}.sudo — generated by sudoc. Traps throw",
        m.name
    );
    let _ = writeln!(
        out,
        "// _rt.SudoTrap; Err results throw _rt.SudoError; invalid inputs throw"
    );
    let _ = writeln!(out, "// TypeError/RangeError (see _sudo_rt.mjs).");
    let _ = writeln!(out, "import * as _impl from \"./{}\";", crate::impl_file(&m.name));
    let _ = writeln!(out, "import * as _rt from \"./{}\";", crate::RUNTIME_FILE);
    let _ = writeln!(out);

    for name in &needed_in {
        emit_named_in_helper(name, m, &mut out);
    }
    for name in &needed_out {
        emit_named_out_helper(name, m, &mut out);
    }

    for f in &adapted {
        emit_wrapper(f, &mut out);
    }

    if !skipped.is_empty() {
        let _ = writeln!(
            out,
            "// Exports without a JS host-boundary mapping (a function-typed value"
        );
        let _ = writeln!(
            out,
            "// somewhere in the signature, or a Result in parameter position) — call"
        );
        let _ = writeln!(out, "// these directly from {}:", crate::impl_file(&m.name));
        for f in &skipped {
            let _ = writeln!(out, "//   {}", f.name);
        }
    }

    Some(out)
}
