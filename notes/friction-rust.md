# Friction log: `backend_rs`

Notes from implementing the Rust backend against `spec/backend-guide.md`, the
JS reference (`backend_js` + `_sudo_rt.mjs`), IR, SDK, and harness. Every open
question from the task brief is answered here.

---

## Environment / process friction

### Shell Execute is unusable for mutations (same as JS)

Mutating shell (`cargo`, `rustc`, redirects) was cancelled in earlier turns;
Write/Edit tools create files (and parent dirs). Verification
(`cargo test`, `sudoc conformance`, etc.) must run outside this agent session.

### Parallel Swift registration

When wiring the harness, `backend_swift` was already in
`sudoc/Cargo.toml`, `harness/Cargo.toml`, and `all_backends()`. Rust was added
**alongside** Swift (not replacing it): order is py, c, js, swift, rs.

---

## Spec / guide open questions (resolved)

### Enum in `needs_dup` (deliberate departure from JS)

JS/Python can share enum values safely because mutation of composites goes
through reference variables and the callee copies non-inout params on entry.
Rust **moves** on assignment/arg-pass of owned values. An `Enum`-typed local
used twice without `.clone()` is a use-after-move compile error.

**Resolution:** treat `Enum` like `Record`/`List`/`Map`/`Set` for `needs_dup`
(always true). Also recurse into `Option`/`Result`/`Tuple` so
`Option<List<int>>` clones at store points while `Option<int>` stays Copy and
needs no clone. Cost: a few extra `.clone()` calls; correctness without
last-use analysis. Documented in `lib.rs` module docs.

### `inout` as native `&mut` (no writeback tuple)

JS returns `[ret?, ...inouts]` and reassigns at the call site. Rust uses
`fn f(x: &mut T)` and call sites pass `&mut local` / `&mut path.field`.
No return-tuple threading. This is strictly simpler than JS and matches the
decision log.

**Local emission:** inout params are tracked in a `HashSet`; `Local` in value
position emits `(*name)`; whole-variable assign emits `*name = …`; field
assign / method recv use the bare name (auto-deref).

### StackOverflow is unobservable

A real Rust stack overflow **aborts the process** (`SIGABRT` / stack overflow
abort); it cannot be caught with `catch_unwind`. No synthetic recursion-depth
counter was added (would diverge from “what the language actually does”).

**Corpus check:** no `conformance/semantics/*.sudo` or `examples/*.sudo` test
exercises unbounded/deep recursion expecting `StackOverflow`. Current trap
tests are Overflow / DivByZero / OutOfBounds / KeyMissing / UnwrapFailed /
InvalidArg / InvalidConvert only. So this limitation does not fail the
present corpus; it remains a known soft spot if a future test relies on
catchable stack overflow (lockstep.md §3 carve-out).

### Float sort — not `f64::total_cmp`

`total_cmp` orders negative NaN before everything (including −∞). Spec §7
wants NaN last, −0.0 before +0.0. Runtime implements JS-faithful
`sort_floats` with `(nan_group, value, sign)` keys and `slice::sort_by`
(stable). `List<int>` uses native `.sort()` (`Ord` on `i64`).

### Float `round` ties away from zero

`f64::round` is not relied on. `round_half_away` uses `floor(x+0.5)` /
`ceil(x-0.5)` like the JS runtime.

### Floor div/mod

Rust `/` and `%` truncate toward zero / follow dividend sign. Runtime
`div` / `mod_i64` convert to floor semantics and trap DivByZero and
`i64::MIN / -1` Overflow — ported from `_sudo_rt.mjs`.

### Evaluation order — and free-function `&mut` (new: Rust joins §4.1)

Rust evaluates function arguments and binary operands left-to-right
(defined). Frontend hoisting still ensures inout-passing calls only appear
as statement roots.

**However**, Rust still needs operand temporaries for a *different* reason
than C. Method calls get **two-phase borrows** (`vec.push(vec.len())`
compiles), but **free-function** calls with an explicit `&mut` argument do
not: `put(&mut a, i, at(&a, j))` is `error[E0502]` even though evaluation
order is well-defined and the code is logically safe. This hit
`insertion_sort.sudo` (`items[i + 1] = items[i]` → `put(items, …, at(&*items, i))`).

**Resolution:** before any call that takes `&mut` to a place (`sudo_rt::put`,
every multi-arg mut-builtin, inout `CallFunc` args), materialize every
*other* operand into a named `let` temporary first (L→R), and only then
construct the `&mut`. Same shape as C’s temporary materialization in
backend-guide §4.1, but forced by the borrow checker rather than unspecified
evaluation order. **Guide update for next backends:** add Rust (and likely
Zig) to the “needs temporaries” side of §4.1 with this two-phase-borrows
explanation.

### Nested `format!` brace-escaping in codegen

Generating `format!("{\"r\": ...}")` *inside* an outer `format!` in the
emitter double-counts brace escaping and produced invalid format strings in
the output (single braces → `expected '}', found '\"'`). **Resolution:**
SudoCanon bodies for records/enums with fields use string concat
(`[prefix.to_string(), fields.join(", "), suffix.to_string()].concat()`),
never an inner `format!`. Nullary/empty branches stay as plain
`"...".to_string()`.

### Operator precedence (Rust, not JS)

Audited against Rust’s grammar (not JS’s table):

| tier | ops |
|------|-----|
| 1 | `\|\|` |
| 2 | `&&` |
| 3 | all six comparisons (non-associative) |
| 4 | `+` `-` |
| 5 | `*` `/` `%` (floats only; ints go through runtime helpers) |
| 6 | unary `!` `-` |
| 9 | atom |

Comparisons share one non-associative tier — chained `a < b == c` is illegal
in both sudo (checker) and Rust (`rustc` rejects). Over-parenthesize when
unsure (guide §4.13).

### Host-builtin shadowing (§4.11)

**Does not bite the same way as JS.** Codegen always uses fully-qualified
`std::collections::HashMap` / `HashSet` and `crate::sudo_rt::…`, never bare
`use` imports that a user type could shadow.

**stdlib name search:**

| name | in stdlib? | clash? |
|------|------------|--------|
| `BigInt` | `stdlib/bigint.sudo` record | No Rust built-in `BigInt` type in prelude |
| `Vec`, `Map`, `Set`, `HashMap`, `HashSet` | not declared as user types | — |
| `Option`, `Result`, `Some`, `None`, `Ok`, `Err` | language builtins only | reserved in sudo |

No corpus case requires extra escaping beyond fully-qualified paths.

### Test-runner path / realpath (§4.12)

JS-specific (`import.meta.url` vs argv realpath). Rust binary has a single
`main`; no double-invocation guard needed. Execute tests compile with
`rustc` in a temp dir and run `./sudo_tests`.

### `for i = a to b` at `i64::MAX`

Inclusive range via `i128` cursor:

```rust
for _sudo_i128 in (from as i128)..=(to as i128) {
    let mut i: i64 = _sudo_i128 as i64;
    // body
}
```

`i128` can represent `i64::MAX + 1`, so the range is well-formed and a single
iteration at MAX terminates. `continue` advances the iterator correctly (no
hand-rolled while bookkeeping). Descending uses `.rev()` on
`(to as i128)..=(from as i128)`.

### Module layout / `#[path]`

Flat dir, one `.rs` per sudo module, `mod sudo_rt;` on entry only, and
`#[path = "dep.rs"] mod dep;` for each import (every module emits path attrs
for its own direct imports). Cross-module IR names `dep.fn` → `dep::fn`.
Corpus today has no `import` (lightly exercised path).

### Hashability derives

Always `Clone, PartialEq`. Also `Eq, Hash` iff the structural walk matching
`sudoc_types::is_hashable` says so (reimplemented against `IrRecord`/`IrEnum`
without depending on the `types` crate). Float fields correctly block
`Eq`/`Hash` — rustc is the fast signal if the walk is wrong.

### Enum payload boxing

Same rule as `backend_c::types_gen::boxed_in_payload`: box
`Record` / `Enum` / `Option_` / `Result_` / `Tuple` fields inside enum
variants. Construction wraps `Box::new`; match unboxes with
`let binder = *tmp;`. Lists/maps/sets are not boxed (already heap-indirect).

### Canon / assert diagnostics

`SudoCanon` trait + free `canon`; generated impls for each record/enum.
List form uses comma-space separators so failing-assert detail contains
`[1, 3, 999]` as required by harness lockstep tests. Map/Set iteration order
in canon is native (diagnostic-only per lockstep v1). Floats force trailing
`.0` for integral values.

### Traps via `panic_any` + `catch_unwind`

`SudoTrap { kind, detail }` with `panic_any` (never plain `panic!("…")`).
Test runner installs a no-op panic hook so expected traps don’t spam stderr.
`expect_trap` uses `catch_unwind(AssertUnwindSafe(|| { body }))` and
downcasts; wrong kind / no panic → `AssertFailed` with diagnostic detail
(mirrors JS).

### Module constants

Corpus has no module-level `const`. Emitter uses
`pub(crate) const NAME: Ty = expr;` first; scalars only are expected per
language restrictions. If a future const expression is not const-evaluable in
Rust, fall back would be a zero-arg fn — not needed today.

### Value-semantics call-site clones

Non-inout composite arguments are `store()`’d at the **caller** (Rust cannot
copy on callee entry without moving the caller’s binding). Fresh values
(literals, constructors, call results) skip clone when not `aliasing`.

---

## Guesses made where guide/SDK/IR were silent

1. **Always type-annotate `let mut` declares** (`let mut x: Ty = …`) so
   `vec![]` / `HashMap::new()` infer. IR gives `value.ty`.
2. **All owned params are `mut`** so reassignment and method mutation of the
   local copy compile without analysis.
3. **`crate::sudo_rt::` from every module** (not bare `sudo_rt::`) so dep
   modules resolve the runtime declared only on the entry crate root.
4. **Native `==`/`!=`** for all PartialEq types (including composites) —
   Rust’s derived/structural PartialEq matches IEEE NaN and deep structure;
   no separate `eq` walker like JS.
5. **Result get_or** uses `unwrap_or_else(|_| default)` (not `||`) because
   of the `FnOnce(E) -> T` signature.
6. **Tuple reassignment** materializes through temporaries (Rust has no
   multi-assign to existing names).
7. **SudoCanon for tuples** implemented up to arity 4; corpus uses pairs
   primarily.

---

## What would have helped next backends (Swift/Zig already queued)

- Explicit table: “languages where callee-entry copy works (ref semantics)
  vs caller-must-clone (move semantics).”
- Note that `StackOverflow` is optional per target and which corpus tests
  (currently none) depend on it.
- Confirm `#[path]` flat multi-module layout once an `import` fixture lands
  in conformance.
- Prefer “always fully-qualified host std paths” as a hard rule in §4.11
  (Rust validates the rule even when JS’s global shadowing doesn’t apply).

---

## Files delivered

**Created**

- `sudoc/crates/backend_rs/Cargo.toml`
- `sudoc/crates/backend_rs/src/lib.rs`
- `sudoc/crates/backend_rs/src/runtime/sudo_rt.rs`
- `sudoc/crates/backend_rs/tests/emit.rs`
- `sudoc/crates/backend_rs/tests/execute.rs`
- `notes/friction-rust.md`

**Modified (rs added alongside existing swift)**

- `sudoc/Cargo.toml` — workspace member `crates/backend_rs`
- `sudoc/crates/harness/Cargo.toml` — dep `sudoc-backend-rs`
- `sudoc/crates/harness/src/lib.rs` — `RsBackend` in `all_backends()`

**Not touched:** spec/, conformance/, other backend crates, cli.
