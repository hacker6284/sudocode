# v0.7 — Nominal types across module boundaries

Status: design, approved 2026-08-15. Implements the fix for the v0.6.0 bug where
`sorting.sort_by(xs, less)` fails when `xs: List<Thing>` and `Thing` is a
caller-declared `record`. v0.6.1 ships a loud call-site rejection; this replaces
it.

## The problem

A generic defined in one module cannot be instantiated at a type declared in
another. Established, each verified against the code:

- Instantiations are generated into the DEFINING module (`spec/language.md:514`;
  `drain_worklist` pushes into `p.ir.funcs`, `types/src/lib.rs:611`).
- The instantiated body is checked against the DEFINER's context
  (`func_check::check_func(&concrete, &p.ctx, &p.type_names)`, `lib.rs:610`).
- `ty_to_type_expr` (`lib.rs:104-129`) renders `Ty::Record("Thing")` as a bare
  `TypeExpr::Named{None,"Thing"}`, which cannot resolve in the definer's scope
  (`lib.rs:1242`, "unknown type 'Thing'").
- Every static backend emits record/enum decls only from its own module
  (`backend_zig/src/lib.rs:268`, `is_portable` at `:3122`;
  `backend_c/src/types_gen.rs:163,371`).

On v0.5.0 the same program passed `check` and built on py, but PANICKED
backend_zig at `lib.rs:448`. It never worked on the static backends: a checker
hole, closed accidentally by 3a547c4 (merge sort) because the new body names `T`
where the old insertion sort did not.

Two dead ends, both tried, both recorded so nobody retries them:

- **Extending `type_names` for the instantiation.** No effect: `check_func`'s
  `type_names` parameter is DEAD — `let _ = type_names;` at `func_check.rs:71`.
  Body annotations resolve through `ctx.records`/`ctx.enums` inside `FnChecker`.
  There is exactly ONE resolution path to fix, which makes the real fix smaller
  than it looks.
- **Writing stdlib generic bodies without naming `T`** (drop `buf: List<T>`,
  `out: List<T>`). `check` then passes and zig STILL panics. This is a trap: it
  restores the py/js-only illusion behind a documented API.

## The model

**A program-global nominal type space, materialized as a synthetic leaf module
`sudo_types`.** Three rules:

- **R1 — Global identity.** A nominal type is identified by (declaring module,
  name). The frontend assigns each decl a program-unique IR symbol: the bare
  name when unique program-wide, else `mangle::qualify_type(module, name)`
  applied to EVERY colliding decl (so the symbol does not depend on which module
  is the entry). `Ty::Record(String)` then becomes globally unambiguous.
- **R2 — Escape analysis and placement.** A decl escapes if any other module
  references it. The shared set is the escaping set closed under record-field and
  enum-payload types. Escaping decls move out of their home `IrModule` into a
  synthetic `sudo_types` module (empty imports/consts/funcs/tests), placed FIRST
  in `modules`. Every module referencing a shared type gets `"sudo_types"`
  prepended to its `imports`.
- **R3 — Instantiation placement is UNCHANGED.** Instantiations still land in the
  defining module. `InstState`, its sharing via `DepExports.inst`, and
  `language.md:514` all stay.

R3 is the payoff: the problem was that the definer's UNIT could not name a type
from the caller's UNIT. A leaf types-unit everyone can import removes that
without moving any code generation.

Readability, an explicit product value, is preserved: a program with no escaping
types emits byte-identical output to today. `sudo_types.*` appears only when a
type actually crosses.

### Rejected alternatives

- **Instantiate into the caller (transitive closure).** The template's body
  resolves in the template's scope, so moving the instantiation drags the
  module-local call graph with it — `sort_by_key` calls `sort_index_of_keys` and
  `apply_permutation` (`stdlib/sorting.sudo:175-176`) — forcing private helpers
  to be exported, changing `InstState` from per-definer to per-caller (one copy
  becomes N), and still failing the mirror case where a template constructs a
  DEFINER-local record. Solves nothing (b) does not, and costs readability.
- **A dedicated instantiations module.** This is the same idea with functions
  moved instead of types, and strictly worse: instantiated bodies call the
  definer's private helpers, so you re-derive caller-placement plus an extra
  file. Types are what must be shared; functions are not.
- **Always-qualify every nominal type.** Correct, wrecks readability in the 99%
  case, churns every golden.
- **Per-node module tags** (`Ty::Record { module, name }`). Honest, but a
  full-width IR change touching every backend and every `Ty` match arm to buy
  what R1 buys with a uniquing pass. Rejected on cost, not principle — this is
  the escape hatch if R1's collision rule proves unstable.

## Per-backend obligations

Every backend already receives the whole program (`emit_program(modules)` for
py/js/rs/zig, `merge()` for c/swift, `allMods` for hs), so none needs new inputs.

| Backend | Shared unit | Change |
|---|---|---|
| py | `_sudo_types_impl.py` | qualify `NewRecord`/variant refs to the owning unit (today emits a bare `name(...)`, `backend_py/src/lib.rs:519`); needs a program-wide name→unit index |
| js | `_sudo_types_impl.mjs` | same as py (`backend_js/src/lib.rs:86`) |
| zig | `sudo_types.zig` (exists) | DELETE `is_portable` (`:3119`) — the hoist set comes from the IR, not a predicate; fix the `self.m.record(n).expect(...)` family (`:448` is the known panic); `topo_order` must take the program |
| c | none (single TU) | `Renamer::type_name`/`type_ref` become identity (`program.rs:125-135`) — the frontend already uniqued; order decls topologically across the merged set |
| swift | none (single file) | as c |
| **rs** | `sudo_types.rs` | **the one structurally broken backend.** Each importer emits `#[path] mod dep;` (`:78-81`), so a diamond compiles a module twice and every shared type becomes two incompatible copies. Fix: ENTRY emits `mod` for every module; non-entry emits none; `qual_name` → `crate::dep::f`. `conformance/semantics/std_imports.sudo` ALREADY forms a diamond (main→regex, main→strings, regex→strings) |
| hs | `T_sudo_types.hs` | straightforward BECAUSE `sudo_types` is a leaf — the alternative (definer imports caller) is a GHC import cycle needing `.hs-boot`. Verify `mangleModule "sudo_types"` is legal and non-colliding |

Fallback if rs's flat crate proves painful: emit rs as a single merged file like
c/swift. Do not take it unless forced — it costs rs's per-module readability.

## Checker change

**Stop substituting type arguments through surface syntax.** Keep the template
AST intact and check the instantiation with a pre-resolved generic environment:
`resolve_type_with` already takes `gmap: HashMap<String, Ty>` (`lib.rs:1190`) and
does exactly this at call sites. Thread a `gmap` into `FnChecker` and use it at
every body annotation site. **`ty_to_type_expr`, `subst_type_expr` and
`subst_stmts` are deleted.**

One consequence: `IrParam.boundary` is computed from the substituted surface AST
(`func_check.rs:57`); for instantiations it must derive from the resolved `Ty`,
since the AST still says `T`. Expect this to surface as `BoundaryTy::Named("T")`
on the wire, caught by the hs decoder rather than by anything in Rust.

## Protocol

**Bump 3 → 4.** The JSON shape does not change; the INTERPRETATION does. A
protocol-3 backend fed a v0.7 program emits code that compiles and is silently
wrong for py/js (unqualified constructors) — exactly what protocol.md §3's
exact-match rule exists to prevent. Bump `sudoc_sdk::PROTOCOL_VERSION`
(`sdk/src/lib.rs:42`) and the `VNum n | n == "3"` guard
(`backends/haskell/Emit.hs:156`) in the same commit.

An external backend must: accept `"protocol": 4`; expect a module literally named
`sudo_types` and emit it like any other; treat `imports` as UNIT dependencies
rather than a transcript of source `import` statements; and resolve a nominal
type's home unit via the program-wide decl table rather than assuming
`Ty::Record(n)` is declared in the module being emitted. That last point is the
whole obligation, and it is the same lookup rs already does for functions.

Also add the missing guard: `is_module_ident` (`types/src/lib.rs:289`) must reject
`mangle::is_reserved` names so no user module can be called `sudo_types`.

## spec/language.md §9 wording

Replace the bullet at `:514-517`:

> - **Importable:** functions (including generic ones) and constants.
>   Instantiations of an imported generic are generated into the **defining**
>   module — a template's body resolves in the template's scope, which is why
>   they cannot be generated at the use site.
> - **Nominal types have program-global identity.** A `record` or `enum` is
>   identified by (declaring module, name). Two modules may declare types with
>   the same name; they are distinct and never unify. The compiler assigns each
>   a program-unique generated symbol, and backends place types that cross a
>   module boundary into a shared compilation unit every module may reference.
> - **A caller-declared nominal type may be used as a type argument to an
>   imported generic.** `sorting.sort_by(xs, less)` with `xs: List<Thing>` is
>   legal. Inside the template the type parameter stays opaque: the body may
>   store, copy, compare and pass values of it and use it in any container, but
>   cannot name, construct, or access fields of it — none of which is
>   expressible against a type parameter anyway.
> - **Still rejected in v0.7:** a CONCRETE imported function whose signature
>   mentions a module-local record or enum, and naming a foreign type in an
>   annotation (`dep.Thing`).

`sig_portable` (`lib.rs:72-89`) survives v0.7 unchanged — it guards
`check_module_call`'s concrete path (`func_check.rs:1814`) only, and the generic
path never consulted it, which is why the hole existed. Rename it to
`sig_nominal_free` while it lives: the current name asserts a property the design
no longer has.

## Mangling and identity

- **Instantiation identity is (defining module, template, type args)** —
  unchanged and already correct. `InstState` shared via `DepExports.inst`
  (`lib.rs:67`) plus the `inst.sigs.contains_key` dedup means two modules
  instantiating `sort_by` at `Thing` get exactly ONE copy, in `sorting`.
- **`mangle_ty` needs no grammar change** given R1: `Ty::Record(n) => n.clone()`
  (`mangle.rs:140`) is correct once `n` is globally unique. But a qualified
  symbol contains `_`, which `mangle_ty`'s doc comment currently argues cannot
  happen ("the checker bans `_` in every record/enum name"). **Update that
  argument:** the ban is on USER-declared names; generated qualified names are
  self-delimiting by construction (`Sudo_M<len><mod>_<len><name>`), a different
  and still-sound uniqueness argument. Write it down or a future reader will
  think the invariant is broken.
- **`mangle_check` gains a job.** It currently compares `Ty` values
  (`mangle_check.rs:59`), so two independent `Thing`s in two modules are ALREADY
  indistinguishable in the IR — harmless today only because nominal types never
  cross, and the hardest constraint on this design. It must run after R1 and
  assert no two decls share a symbol.
- **Raise the runaway guard.** `*count > 32` (`lib.rs:582`) counts distinct
  instantiations per template program-wide; with nominal type args legal, 32
  distinct instantiations of `sorting.sort_by` is reachable honestly. Raise to
  ~256 and re-scope to detect GROWING type arguments (the actual
  polymorphic-recursion signature). The current message ("recursive generic
  instantiation is not supported") will be wrong for the first legitimate user.

## Migration

Consumers of `sudoc`: nothing to change; programs rejected by v0.6.1 now compile,
and no previously-accepted program is rejected. External backends must ship a
protocol-4 update in the same change (protocol.md §6). Bazel needs no rule change
(`declare_directory`, `rules_sudo/private/rules.bzl:62`).

One silent change to document in the release notes: where two modules declare the
same nominal name AND at least one escapes, both symbols change from `Thing` to
`Sudo_M…`. That symbol appears in `canon` output, which is what assertion-failure
diagnostics print. Behaviour is unchanged; diagnostic TEXT changes — and this is
correct, since with two distinct `Thing`s live a diagnostic saying `{"r":
"Thing"}` was already ambiguous. Also mention the new `sudo_types.*` file in the
generated set; people read this output.

## Tests

`conformance/semantics/generics.sudo` covers int and float ONLY. Zero
nominal-type coverage is why this survived to a release. The replacement is a
matrix, per the house rule (enumerate from the type definition, always write the
mirror).

Core grid **B × A**:

- **A. Nominal kind:** record | flat enum | enum with payload | recursive enum |
  record with a nominal field (exercises the closure rule)
- **B. Position in the signature:** `T` | `List<T>` | `inout List<T>` |
  `Option<T>` | `Map<int,T>` | `Map<T,int>` (hashable path) | `Set<T>` |
  `(T,int)` | `func(T,T)->bool`

Enumerated once each against a representative B row (full cross is ~10k programs
and buys nothing):

- **C. Declarer:** entry | non-entry dep | (negative) stdlib module
- **D. Definer:** entry | file dep | stdlib module
- **E. Instantiation count:** one caller | two callers same type (assert ONE
  emitted symbol) | two callers different types
- **F. Name collision:** unique | same name in two modules both escaping | same
  name, one escaping
- **G. Import topology:** linear | **diamond** — catches the rs `mod` tree, and
  should exist standalone regardless of generics
- **H. Template internals:** leaf | calls a module-local private generic helper |
  `==` on `T` | `T` as a Map/Set key | (later) returns a definer-local record

Negative matrix, each pinned to its exact message: concrete cross-module call with
a nominal signature; `dep.Thing` in type position; the instantiation-count guard;
the mangle collision oracle.

Oracles: 7-backend lockstep (primary); goldens reviewed for readability
(`sudo_types.*` ABSENT when nothing escapes, non-escaping types stay home); and a
`check`-only test asserting the SHAPE of the emitted `modules` array so placement
regressions fail in the frontend rather than in seven backends at once.

Regression pin: the exact bug-report program, plus the `sort_by_key` variant
(which exercises H's transitive-helper row).

## Staging

0. **Frontend only** — R1 + escape analysis + `sudo_types` synthesis + protocol
   bump. Zero backend changes; verify with a `check`-level snapshot of the module
   array. Nothing escapes yet, so all existing programs are unaffected. Lands the
   risky whole-program reasoning alone, where it is cheap to debug.
1. **Checker fix** — delete `ty_to_type_expr`/`subst_*`, thread `gmap`, fix
   `IrParam.boundary`. `check` passes on the regression program; backends still
   fail. Include py/js constructor qualification here (small) and gate on them.
2. **rs flat crate** — independent of 0 and 1, fixes an existing latent diamond
   bug. Consider landing FIRST; its test is the diamond program, not generics.
3. **zig** — delete `is_portable`, hoist set from the IR, fix the
   `self.m.record(...).expect()` family and `topo_order`. Highest effort, most
   visible win (it is what panics today).
4. **c + swift** — mostly deletion (`Renamer` becomes identity) plus topological
   decl ordering.
5. **hs** — external and slowest to iterate, but structurally easiest here (leaf
   module, no cycles). Must land with the protocol bump.
6. **Docs** — §9, protocol.md, `stdlib/README.md`; drop the v0.6.1 rejection.

Deliberately NOT in v0.7: killing `sig_portable` — qualified annotations
(`dep.Thing`), foreign field access, foreign construction, foreign variant
patterns. That is a language-surface change with its own questions (variant-name
ambiguity across modules, `DepExports` gaining records/enums). Keeping it out is
what makes v0.7 finishable.

## What will go wrong

1. **The rs diamond**, if stage 2 is skipped. `std_imports.sudo` already is one.
2. **The `self.m.record(name).expect(...)` family.** `backend_zig:448` is the
   known panic; there will be more. Grep `\.record(` and `\.enum_(` across all
   backends — each is a site that assumed nominal types are module-local. Prefer
   changing the lookup helper's signature to take the program so the compiler
   finds them all for you.
3. **Declaration ordering in the shared unit.** C needs records ordered
   topologically by field dependency; a right closure with a wrong order produces
   "incomplete type" errors that read like a missing-decl bug.
4. **`reserved::rename_reserved` runs per module.** If `sudo_types` is renamed
   independently of the modules referencing it, a record named `class`/`type`
   gets two different names. Route the shared unit through the identical pass in
   the same batch.
5. **Hashable foreign types.** `Map<Thing,int>` needs `canon`/`keyapp`/`hash` in
   the shared unit, and `is_hashable` (`lib.rs:1251`) consults `ctx` — the WRONG
   module's context for a foreign record. A checker bug that will present as a
   backend crash. Check it before you need it.
6. **`boundary` on instantiated params** — see the checker section.
7. **The 32-instantiation guard** firing on an honest program.
8. **hs is external and slow.** Run stages 0–1 with hs pinned at protocol 3 and
   EXPECTED to fail; land hs with the bump. Do not try to keep it green between.
9. **Escape analysis under-approximating.** The obvious first implementation
   scans `Ty::Record`/`Ty::Enum` in funcs/consts/tests and forgets one of
   `Place::Index.base_ty`, `Place::Field.base_ty`, `MutBuiltin.recv_ty`,
   `IrPattern::Variant.enum_name`, or `IrExpr.ty` on interior nodes.
   `mangle_check`'s walker (`mangle_check.rs:155-255`) is already the complete
   traversal — REUSE it, and make the reuse explicit so the two cannot drift.
