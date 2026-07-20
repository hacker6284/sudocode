# Friction log: `backend_swift`

Notes from implementing the Swift backend against `spec/backend-guide.md`, the
C/JS reference backends, IR, SDK, and harness. Authoring was file-by-file via
Write/Edit without a local compiler; every claim below was independently
verified by the wrapper across several fix-and-reverify rounds (`cargo build`,
`cargo test` — all 22 `backend_swift` tests green; `cargo clippy -p
sudoc-backend-swift --all-targets` — zero warnings; `cargo build --release`;
`sudoc conformance --target py --target c --target js --target swift` — 9/9
modules green; `sudoc test` over `examples/*.sudo`, `examples/pitfalls/*.sudo`,
and `stdlib/*.sudo` across py/c/js/swift — fully green except the expected
`order_dependent` divergence).

---

## Environment / process friction

### Shell Execute tool is unusable for mutations in this sandbox

Same constraint as `notes/friction-js.md`: no `cargo`, `swiftc`, `mkdir`, or
redirects in the agent session. All sources were created with Write/Edit tools
only. Verification ran outside the authoring turn.

### swiftc wall times (measured)

Representative single module (`bst.sudo` — 3 tests, one record + one recursive
enum): `swiftc -parse-as-library -o sudo_tests bst.swift sudo_rt.swift` ≈
**0.75s** wall; the binary is near-instant (~0.2s process startup, sub-ms test
execution). Full `tests/execute.rs` suite (8 real `examples/*.sudo` + 6
hand-written edge-case programs — 14 compile+run cycles) ≈ **8.8s** wall total
(~0.6s/cycle). Same-run comparison: backend_c 9-test execute suite ≈ 2.3s;
backend_js 7-test suite (no compile) ≈ 0.44s.

**Verdict:** `swiftc` is noticeably slower per artifact than C or an
interpreted target, but well under a second per module — `sudoc conformance` /
`sudoc test` stayed comfortably fast on the full 9-module conformance corpus
and the examples+pitfalls+stdlib lockstep run (8 examples + `order_dependent`
+ bigint + sorting + strings). Whole-module compilation of one merged file is
doing real work; if the corpus grew 10–50×, `swiftc` wall-clock is the first
thing to profile, but it is not a problem at current size.

### Shared workspace note

Verification ran in a working tree that also contained an in-progress,
unrelated `sudoc-backend-rs` crate (registered in the same `all_backends()`
list by a separate lane). Several of that crate's generated-code tests fail on
a pre-existing `format!`-escaping bug untouched by this task — so plain
`cargo test` / `sudoc conformance` (no `--target` filter) show `rs` failures
alongside full `swift` success.

---

## Simplifications vs C/JS — confirmed

These six claims are the structural bet of the backend. Full conformance plus
`value_semantics.sudo` / `structures.sudo` / `floats.sudo` specifically
exercise claims 1–3; all passed.

| # | Claim | Verdict | Why |
|---|--------|---------|-----|
| 1 | Value semantics native — no dup/free | **Confirmed** | `struct`/`enum`/`Array`/`Dictionary`/`Set` are COW value types. Assignment `b = a` then `b.append` leaves `a` unchanged (`value_semantics.sudo`). No `dup()` ported from JS, no ownership scopes from C. |
| 2 | Deep `==` native | **Confirmed** | Synthesized `Equatable` on records/enums/tuple structs; `Array`/`Dictionary`/`Set` element-wise `==`. IEEE `Double` (`NaN != NaN`, `-0.0 == 0.0`). Dict/Set equality order-insensitive — covers `structures.sudo` map/set equality and `floats.sudo` NaN-in-composite. No `sudoEq` wrapper. |
| 3 | `for x in c` snapshot free | **Confirmed** | Bind `let _sudoIterN = try <expr>` then iterate the binding. COW: mutating the original var cannot change the snapshot's storage. Maps use `for (k, v) in dict`. |
| 4 | Map index-assign is insert-or-overwrite | **Confirmed** | Emit `m[k] = v` (native `Dictionary` subscript setter). No `_put` helper. |
| 5 | `filled(n, v)` is `Array(repeating:count:)` | **Confirmed** | After `n < 0` → `InvalidArg`. COW shares initial storage until element mutation. |
| 6 | Generic runtime, not monomorphized helpers | **Confirmed** | `listAt`/`mapAt`/`chkAdd`/… written once in `sudo_rt.swift`. Only per-shape codegen: tuple structs + user record/enum decls. |

---

## Bugs found and fixed during verification

### `@main` + file named `main.swift` → `-parse-as-library`

Multi-module import lockstep entry module is literally named `main`, so the
merged artifact is `main.swift`. Swift treats a file named `main.swift` as
implicit top-level code; `@main` then fails with
`'main' attribute cannot be used in a module that contains top-level code`
(compiler suggests `-parse-as-library`).

**Fix:** add `-parse-as-library` to the `swiftc` invocation in both
`test_recipe()` and the execute-test harness. Safe unconditionally: every file
handed to this invocation is always a `with_tests: true` build with exactly
one `@main`.

### Blanket param shadowing broke closure-typed parameters

Every non-`inout` parameter was shadowed with `var name: T = name`. That
breaks closure-typed (`func(...)`) params: Swift closures are non-escaping by
default and cannot be assigned into a `var` of the same type
(`using non-escaping parameter 'less' in a context expecting an '@escaping'
closure`). Surfaced in `generics.sudo` and `stdlib/sorting.sudo`'s
comparator-taking functions.

**Fix:** only shadow a parameter with `var` when the function body actually
mutates it (direct reassignment, tuple-assign target, mutating-builtin
receiver, or forwarded into another call's `inout` slot) — a
`mutates_local`/`place_root` walk mirroring backend_c's `uses_local` shape.
Also eliminated a pile of "variable was never mutated; consider changing to
'let' constant" warnings on every untouched parameter across the corpus.

### `order_dependent.sudo` needs the C-style non-assert special case

Swift's per-process-random `Dictionary` iteration order makes that file's
"DIVERGES" test flip between pass and trap run to run — confirmed across
repeated runs, exactly the behavior backend-guide.md §4.8 predicts. Asserting
it must always pass made backend_swift's own test suite flaky.

**Fix:** same compile-and-run-but-don't-assert-pass-on-that-one-test special
case `backend_c/tests/execute.rs` already uses.

### `is_swift_keyword` over-escaped `some`/`any`

Emitted `case .`some`(let v):` — but `some`/`any` are *contextual* keywords
valid unescaped outside type position (Swift's own `Optional` declares
`case some(Wrapped)` with zero escaping).

**Fix:** remove them from the escape list; output is now consistent with
`sudo_rt.swift`'s own bare `case some(T)` declaration.

### Clippy (emitter, one-line)

Three minor clippy fixes on the Rust side: a useless `format!`, an `if`/`else`
with identical branches, a needless explicit lifetime.

---

## Land-mine catalog (backend-guide §4) applied to Swift

### 4.1 Evaluation order

Swift specifies left-to-right evaluation for operators and call arguments
(similar to JS). No C-style temporary materialization for order. Expressions
are pretty-printed directly. Inout-passing calls are statement roots only
(frontend hoist), so writeback needs no multi-value return threading.

### 4.2 Short-circuit + effects

Native `&&` / `||`. Frontend hoists inout-passing calls out of short-circuit
operands; `hoisting.sudo` / execute test "short circuit skips mutation" is the
gate.

### 4.3 inout

Checker `inout_root`: only bare locals or record-field chains — **never**
list/map index. Confirmed in `func_check.rs` (`Var` / `Field` only for the
root check path used by inout args). Emit native `inout` + call-site `&x` /
`&rec.field`. No bounds-check-then-subscript trick needed for inout.

### 4.4 Equality

Native structural `==`. Unlike Python, no list identity short-circuit to
defeat. Floats via `Double.==`. No bool/int conflation (separate types).

### 4.5 Loop lowerings

- **`for i = a to b`**: `SudoRange` lazy iterator — continue-safe (state
  advanced in `next()` before the body), wrap-safe at `Int64.max`/`min` via
  `&+`/`&-` only after marking `done` so the wrapped value is never yielded.
  Hand-rolled `while` was rejected: Swift `continue` jumps to the condition
  and would skip a trailing increment (unlike C `for` update clauses).
- **`for x in c`**: snapshot binding + labeled `for-in`.
- **`break`/`continue` inside `match`**: Swift `switch` does **not** capture
  loop break/continue. Still label every loop unconditionally (`_lN:`) and
  emit `break _lN` / `continue _lN` for readable, uniform exits.
- **i64 MAX single iteration**: execute test `for_range_to_i64_max_terminates`.

### 4.6 Traps

`SudoTrap: Error` with kind strings matching py/c/js. `expect_trap` uses
`do/catch`; "nothing trapped" throw is **outside** the `do` so the catch
cannot re-swallow it. No stack-depth counter; `StackOverflow` not synthesized
(same as C).

### 4.7 Numbers

Checked Int64 via `addingReportingOverflow` etc. Floor div/mod ported from C
helpers. Floats: bare `+ - * /`; `sudoFmin`/`sudoFmax` (NaN propagate;
`min(-0,0)==-0`); `rounded(.toNearestOrAwayFromZero)` for ties-away-from-zero;
`intOfFloat` compares truncated Double against `±9223372036854775808.0` before
converting (mirrors `sudo_int_of` — never crash on out-of-range).

### 4.8 Hashing and iteration order

Native `Dictionary`/`Set`. Iteration order is per-process randomized on Apple
platforms — **confirmed** divergence on `order_dependent.sudo` (feature, not
bug): the DIVERGES test flips between pass and trap across repeated runs.
Keys must be sudo-hashable: we declare `Hashable` only when a ported
`is_hashable` (coinductive on record/enum cycles) says so. `[Int64]` and
records/enums with hashable fields are `Hashable` natively. Synthesized
`Hashable` on recursive enums with `indirect case` worked as designed.

### 4.9 Names

`sudoc_ir::names::test_fn_names` for TAP names. Cross-module merge uses
**double** underscore `module__sym` (task design; C uses single `_`). Runtime
temps `_sudo*`. Reserved prefix convention.

### 4.10 Operator precedence (Swift, not JS/Python)

Swift precedence groups used (low→high binding strength in the emitter):

| Tier | Operators | Swift group (approx.) |
|------|-----------|------------------------|
| 1 | `\|\|` | LogicalDisjunctionPrecedence |
| 2 | `&&` | LogicalConjunctionPrecedence |
| 3 | `== != < <= > >=` | ComparisonPrecedence (**one** tier — unlike JS split) |
| 4 | `+ -` (also array concat) | AdditionPrecedence |
| 5 | `* / %` | MultiplicationPrecedence |
| 6 | prefix `!` `-` | Prefix |
| 9 | atoms, calls, subscripts, helper calls | — |

**Self-test of the classic JS land-mine:** `not (a == b)` → operand at tier 3,
ctx 6 for `!` → `3 < 6` → parenthesize → `!(a == b)`. Never `!a == b`.
Confirmed correct across the full conformance + examples corpus; no silent
paren bugs observed. Checked Int64 ops are function calls (tier 9), so they
do not reintroduce precedence bugs.

### 4.11 User names shadow host builtins

Host surface kept in the runtime module (`chkAdd`, `sudoFmin`, `SudoOption`,
…). Generated code uses `Int64`/`Double`/`Dictionary`/`Set`/`abs`/`Double(...)`.
If a user record were named `Int64`, it could shadow — corpus does not. Prefer
runtime helpers over free globals where collision risk is real. `stdlib/bigint.sudo`
defines `record BigInt` — we do not call a host `BigInt` (unlike JS).

### 4.12 Test-runner traps

`@main` + two source files (`{entry}.swift` + `sudo_rt.swift`): explicit
`SudoTestRunner.main` calls `exit(runTests(...))`. File named `main.swift`
collides with Swift's implicit-top-level-code rule — fixed with
`-parse-as-library` (see bugs section above). `import Foundation` is present
on both files; the build succeeds either way (harmless dual import; per-file
vs module-wide sharing stays moot).

### 4.13 Readable output

User identifiers preserved (keyword-backticked only when needed). Real
`if`/`while`/`for`/`switch`. Tuple structs named `TupN_…` like C. Minimal
mangling.

---

## Design choices (confirmed)

### `indirect case` recursion

Blanket rule: every user enum case with ≥1 field is `indirect case`;
`SudoOption.some` and both `SudoResult` cases are indirect. Confirmed:
recursive records (`struct Node { var next: SudoOption<Node> }`) and recursive
enums compile without C-style payload boxing. No corpus type required
narrowing the rule.

### `SudoOption` / `SudoResult` vs Swift `Optional`/`Result`

Swift `Optional` flattens nested optionals in some surfaces and would break
`Option<Option<T>>` distinguishability. Own generics keep nesting explicit.
Conditional conformances:

```swift
extension SudoOption: Equatable where T: Equatable {}
extension SudoOption: Hashable where T: Hashable {}
```

Confirmed: compile cleanly with `indirect case`. Case name `some` is left
unescaped (contextual keyword; see bugs section).

### Param reassignment

Swift parameters are immutable. Non-`inout` parameters that the body actually
mutates are shadowed at entry with `var name: T = name` (selective, not
blanket — see bugs section). COW keeps the copy cheap.

### Tuple structs

Swift anonymous tuples are non-nominal and cannot conform to
`Equatable`/`Hashable`. Named `TupN_<mangle>` with positional `init(_ f0:…)`
and fields `var f0…`. Shared by shape across the merged program (not
module-prefixed).

### Multi-module merge

Mirrors `backend_c::program::merge` with `module__` (double underscore).
Entry symbols bare; only entry tests kept. Rename before tuple/hashability
collection.

### Other details that just worked

- `while try cond` / `if try cond` accepted.
- `@discardableResult` + `throws` + `inout` on one signature fine.
- `SudoOption<T>.none` / `.some` type-qualified constructors infer at use sites.
- `Dictionary<K,V>()` empty init works (no need for `[:] as [K:V]`).
- `Mirror`-based `canon` complete enough for nested generics (diagnostic only;
  lockstep compares kinds, not detail strings).

---

## Intentional divergences from C / JS

| Area | C | JS | Swift |
|------|---|-----|-------|
| int | int64_t + helpers | BigInt + chk | Int64 + reportingOverflow helpers |
| value semantics | copy/free | dup at store | native COW |
| equality | monomorphized `_eq` | `_rt.eq` | native `==` |
| inout | pointers | multi-return writeback | native `inout` + `&` |
| match | switch + goto for break | if/instanceof | switch + labeled loops |
| for-range | for+done flag | BigInt for | `SudoRange` Sequence |
| multi-module | single TU `mod_name` | ESM imports | single TU `mod__name` |
| Option | monomorphized struct | Some/None classes | `SudoOption` generic enum |
| host API | `.h` wrappers | omitted | omitted |

---

## IR / SDK notes

- `Backend::name` = `"swift"`.
- `emit_program`: merge → one `{entry}.swift`; `with_tests` only drives entry
  tests + `@main`.
- `runtime_files`: `sudo_rt.swift` via `include_str!`.
- `test_recipe`: `swiftc -parse-as-library -o sudo_tests {entry}.swift
  sudo_rt.swift` then `./sudo_tests`.
- TAP: `ok N - name` / `not ok N - name [Kind]` / `[Kind: detail]`; exit 1 on
  any failure; `# passed/total passed` summary.
- Test names from `sudoc_ir::names::test_fn_names` only.

---

## Files touched

**Created**

- `sudoc/crates/backend_swift/Cargo.toml`
- `sudoc/crates/backend_swift/src/lib.rs`
- `sudoc/crates/backend_swift/src/program.rs`
- `sudoc/crates/backend_swift/src/types_gen.rs`
- `sudoc/crates/backend_swift/src/code_gen.rs`
- `sudoc/crates/backend_swift/src/runtime/sudo_rt.swift`
- `sudoc/crates/backend_swift/tests/emit.rs`
- `sudoc/crates/backend_swift/tests/execute.rs`
- `notes/friction-swift.md` (this file)

**Modified (only these three registry files)**

- `sudoc/Cargo.toml` — workspace member `crates/backend_swift`
- `sudoc/crates/harness/Cargo.toml` — dep `sudoc-backend-swift`
- `sudoc/crates/harness/src/lib.rs` — `SwiftBackend` in `all_backends()`
