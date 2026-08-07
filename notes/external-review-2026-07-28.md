# External review — adversarial bug hunt (2026-07-28)

Provenance: a context-clean adversarial audit of current main (HEAD
1b082ee), re-scoped from the 2026-07-22 review's open items plus fresh
eyes on this session's large changes (Zig honest-arena overhaul, #17
host-boundary layer, regex groups + matcher perf fix). Every finding
below was REPRODUCED before being written down (commands + probe files
noted; probes live under the session scratchpad, with the load-bearing
repro content inlined here). Baseline at audit start: `sudoc conformance`
14/14 across py, c, js, swift, rs, zig, hs with C sanitizers on.

Findings are ranked most-severe first. Clean sweeps are reported at the
bottom so the negative results are auditable too.

---

## Fix log (2026-07-29)

- **F1 FIXED** (9be3b04): Place::Field now routes to the record root's true
  lifetime. Conformance: field_assign_lifetime.sudo.
- **F3 FIXED** (24d62db): C const backing allocated outside the trap-tracked
  live list (sudo_begin/end_permanent). Conformance: const_survives_trap.sudo.
- **F4 FIXED** (9be3b04): js/swift parenthesize unary-minus operands beginning
  with `-`. Conformance: floats.sudo (double unary minus).
- **F5 FIXED** (9be3b04): zig float +,-,* routed through rt.fadd/fsub/fmul;
  float comparisons coerce to f64. Conformance: floats.sudo (literal fidelity).
- **F6 FIXED** (9be3b04): zig discards unused user params.
- **F7 FIXED** (9be3b04): zig AND js wrap each ForRange in its own block. The
  js sibling (redeclared `const _sudo_from_i`) was caught by the new
  conformance module, not the original audit. Conformance:
  param_and_loop_scoping.sudo.
- **F8 FIXED**: nested-Option boundary check generalized to descend into
  record/enum fields and applied to parameter types, not just returns.
  Checker test: export_boundary_restrictions.
- **F12 FIXED**: Result in a non-inout export parameter is now a loud checker
  error (was a silent adapter skip). Checker test: export_boundary_restrictions.
- **F14 FIXED** (doc): stdlib/regex.sudo header documents that `a{,3}` is a
  literal (the one brace shape that diverges from python's `re`).
- **F2 FIXED**: Haskell backend made observationally strict on traps —
  StrictData + bang params + deep-force at binds/args + left-to-right call-arg
  forcing. deepForce routed through Control.DeepSeq.force (NFData) rather than
  serializing via canon: cheaper (no per-bind string build, kernel hs 2.7s) and
  works for all tuple arities. Conformance: trap_strictness.sudo (18/18).
  - **F2 perf watch-item** (Fable review 2026-07-29): `deepForce` traverses the
    whole value at every let/tuple/discard bind, so a container mutated in a hot
    loop — `m[k] = v`, `xs.append(v)` — is O(size) per bind on hs. Same class as
    the deferred F11 `appendL` quadratic; only bites large hs hot loops (kernel
    is fine, 2.7s). If it ever bites: skip deepForce when the RHS provably has no
    buried trap (a container read / an already-forced local), keeping it only for
    trap-bearing constructions. Not worth doing pre-emptively.
  - **Correction (2026-08-07):** the two named examples above are wrong.
    `xs.append(v)` does not route through `wrapDeepForce` at all —
    `SAssign`/`SExpr` dispatch `EMutBuiltin` to `emitMutBuiltin` before
    the deepForce line (`backends/haskell/Emit.hs:1728-1729`,
    `:1751-1752` -> `:1882-1902`), which rebinds the container with a
    WHNF bang only (Emit.hs:1902). `m[k] = v` deep-forces only the RHS
    *value*, not the container: the force lands on the value expression
    (Emit.hs:1734) and is threaded into `emitPlaceSet` (Emit.hs:1736,
    :1392-1414); the O(size) cost at that site is `Rt.putL`'s
    `take`/`++`/`drop` (SudoRt.hs:238-241) — the list representation
    (the F11 item), not deepForce. The genuine deepForce O(size)
    exposure is whole-container `let` binds (Emit.hs:1734),
    whole-container tuple binds (Emit.hs:1742), and container-typed call
    arguments, one deepForce per argument via `emitStrictApp`
    (Emit.hs:1138). Excluded: function-typed values (Emit.hs:1120),
    inout calls (Emit.hs:1726-1727), mut-builtins (Emit.hs:1728-1729,
    :1751-1752), and loop machinery, which is WHNF-only
    (Emit.hs:2010-2011, :2049).
- **F9 FIXED**: factorial(n<0) traps; cross-module bigint limitation documented.
- **F10 DONE (body-level)**: function/test BODIES now accumulate errors — a file
  with N independent body errors reports all N (passes 1-4, i.e. type/record/
  signature/const resolution, stay fail-fast since a broken signature poisons
  downstream). check_module / check_program_inner now genuinely return
  Vec<TypeError> (the API is no longer a facade); the CLI prints them all. The
  deeper signature-level + generic-instantiation accumulation is tracked in a
  follow-up task (#35). Test: checker.rs::body_errors_accumulate_across_functions.
- **F11 PARTIAL**: RTS stack bounded (`-with-rtsopts=-K8m`) so runaway recursion
  traps StackOverflow cleanly instead of OOM-thrashing — verified no new
  divergence (conformance 18/18, examples+stdlib+kernel clean). The quadratic
  `appendL` rewrite (Data.Sequence) is deliberately deferred (invasive; only
  bites million-element inout appends).
- **F13 DONE (real bugs)**: entry module name must be a valid identifier
  (`verify-hs.sudo` -> GHC crash) and the entry must be a .sudo file
  (`good.txt` no longer silently resolves to good.sudo). Test:
  imports.rs::entry_must_be_a_valid_sudo_file. Remaining F13 items are polish
  (arg-parsing edge cases) — left as documented in the finding.
- ALL 14 findings addressed (F1–F9, F12, F14 fully; F10/F11 partial-by-design;
  F13 real bugs).

---

## Findings

### F1 — CRITICAL (zig): record-field assignment bypasses the escape-copy
routing — UAF crashes and silent wrong answers

- Anchor: `sudoc/crates/backend_zig/src/lib.rs:1268-1291` (the
  `IrStmt::Assign` arm: `if let Place::Var(n) = target { …inout/escape
  routing… } else { self.store(value) }`). `Place::Index` targets get
  `place_escapes(base)` + copy into the container's carried allocator;
  `Place::Field` targets fall into the bare `store(value)` else-branch and
  allocate the RHS in `cur_alloc` (callee scratch or loop arena) with no
  regard for where the record root lives.
- Consequence 1 (crash): assigning a composite to a field of an **inout
  record param** builds the value in the callee's `_sudo_scratch`, which
  is `defer deinit()`-freed on return — the caller's field dangles.

  ```
  record Holder
      xs: List<int>

  func fill(h: inout Holder, n: int)
      h.xs = [n, n + 1]

  test "field assign through inout survives callee scratch"
      h = Holder([0])
      fill(h, 41)
      assert h.xs == [41, 42]
  ```
  Repro: `sudoc test --target zig --target py t1.sudo` →
  `zig: no result (runner crashed?)`, py passes. Generated evidence:
  `fill` allocates `_sudo_lst0` with `.alloc = _sudo_scratch.allocator()`,
  assigns `h.*.xs = _sudo_lst0;`, then the scratch arena deinits.
- Consequence 2 (silent wrong answer): same pattern with the assignment
  **inside a loop** in the callee (value lands in the per-iteration loop
  arena) returns corrupted data without crashing:

  ```
  func grow(h: inout Holder)
      for i = 1 to 3
          h.xs = h.xs + [i]
  ```
  called on an inout Holder → Debug build asserts
  `[0, 0, 0] != [1, 2, 3]` — garbage with a plausible shape.
- Consequence 3 (latent): a **loop-carried local**'s field assigned in a
  loop (`h.xs = [i * 10]`, `h` declared outside) leaves the field pointing
  into the loop arena across `rt.loopReset`. Passes today in ReleaseSafe
  *and* Debug purely because the freed arena bytes happen to be intact
  when re-read (the documented DebugAllocator-oracle blind spot). Any
  allocation landing on those bytes turns this into consequence 2.
- All four probes pass on py/c/js/swift/rs/hs — zig-only.
- Why the suite missed it: the conformance corpus's only field
  assignments are scalars (`structures.sudo:21`,
  `module_constants.sudo:75`). No test assigns a composite to a record
  field at all.
- Direction: give `Place::Field` targets the same destination-based
  routing as `Var`/`Index`: resolve the place root; inout root → copy
  into the root's true-lifetime allocator (the old field value's carried
  `.alloc` for containers, the a0c2d18 re-home convention /
  `_sudo_ret_alloc` for non-container composites); loop-external root →
  function scratch. Add a conformance module covering composite field
  assignment through inout, in callee loops, and loop-carried — all seven
  backends should be held to it.

### F2 — HIGH (hs): systemic laziness divergence — traps inside unread
values never fire; trap ORDER follows GHC demand, not spec §12

- Root cause (verified in emitted code): the Haskell emitter forces binds
  only to WHNF (`case E of !x -> …`) while records/enums are emitted with
  LAZY fields (`backends/haskell/Emit.hs` ~2122–2208, no strictness
  annotations) and functions with LAZY parameters. A trap buried inside a
  constructor field or an unused argument survives as a thunk and never
  fires. Scalars are saved only because WHNF of Int64 is full evaluation;
  Map values are saved by accident via Data.Map.Strict.
- Manifestations (each lockstep-DIVERGED, hs alone vs the strict targets;
  re-verified independently of the probing lane):
  - A1 unread trap values: `p = Pair(10 mod 0, 7)` then `assert p.b == 7`
    → hs pass, py (and all strict targets) trap DivByZero. Same for list
    elements, enum payloads, and unused function arguments
    (`ignore_arg(1 mod 0)`).
  - A2 trap-order inversion: `rev(a[9], 1 mod 0)` (callee demands y
    before x) under `expect_trap OutOfBounds` → hs reports DivByZero
    (callee demand order beats §12 left-to-right); and
    `xs = [a[9], 1 mod 0]` with only `xs.length` read traps nothing.
  - A3 the same class buried in nested record→enum→record→list values no
    assertion touches.
- The conformance corpus's traps all flow into asserted values, so
  WHNF-forcing happens to pass it.
- Repro: scratchpad `verifyhs.sudo`; `sudoc test --target hs --target py
  verifyhs.sudo` (from the repo root; see F13) → 2 DIVERGED.
- Direction: `StrictData`/bang every emitted field, bang-pattern every
  parameter, force composite-literal components left-to-right at
  construction. Then add conformance tests with traps in unread
  constructor fields / unused args / dead list elements so every future
  backend is held to strictness.

### F3 — HIGH (c): trap unwinding frees composite module constants and
never rebuilds them — heap-use-after-free on every later read

- Anchors: `sudoc/crates/backend_c/src/runtime/sudo_rt.c:38-43`
  (`sudo_trap` → `sudo_free_all_live()` sweeps const backing storage
  along with dead allocations); `backend_c/src/code_gen.rs:259-264`
  (`sudo_consts_ready` latches true permanently — consts are never
  re-initialized); `backend_c/src/boundary.rs:123-125` (every exported
  wrapper: idempotent `sudo_init_consts()` + setjmp trap recovery — so
  one *recovered* trap poisons all composite constants for the rest of
  the host process).
- Repro (verified, ASan heap-use-after-free; freed at
  `sudo_free_all_live sudo_rt.c:32` ← `sudo_trap sudo_rt.c:41`):

  ```
  NUMBERS = [1, 2, 3, 4, 5]

  test "trap first"
      expect_trap OutOfBounds
          a = [1]
          x = a[5]
          assert x == 0

  test "read constant after trap"
      assert NUMBERS == [1, 2, 3, 4, 5]
  ```
  `sudoc build --target c --tests -o gen const_trap.sudo && clang
  -std=c11 -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=all
  -o t gen/const_trap.c gen/sudo_rt.c -lm && ./t`
- The trigger is not exotic: a successful `expect_trap` or any ordinary
  *failing assert* poisons every composite constant for all subsequent
  tests in the binary. Also verified at the host boundary (export reads
  const → other export traps and recovers via the documented
  `sudo_trap_status` path → first export again = UAF). In an unsanitized
  production build this is silent stale-read corruption, not a crash.
- Why conformance is green: `traps.sudo` has no composite module
  constants; `module_constants.sudo` has no trapping test.
- Direction: allocate const backing storage outside the tracked live
  list, or reset `sudo_consts_ready` (and rebuild) on trap unwind. Add a
  conformance module combining a composite constant with an earlier
  expect_trap test.

### F4 — HIGH (js) / MEDIUM (swift): `-(-x)` on floats emits `--x` —
JS silently computes a PRE-DECREMENT; Swift fails to compile

- Anchor: `sudoc/crates/backend_js/src/lib.rs:649-658` — float
  `UnaryOp::Neg` emits `format!("-{x}")` where the operand rendering can
  itself start with `-` (nested unary minus on a variable, or the folded
  literal `-0.0`). `--a` in JS is pre-decrement: valid sudo `a = 1.5` /
  `b = -(-a)` produces `let b = --a;` → b == 0.5 AND a mutated. Verified:
  single-target js run fails `line 4: {"f": "0.5"} != {"f": "1.5"}` —
  silent wrong answer. The literal form `c = -(-0.0)` emits
  `let c = --0.0;`, a load-time SyntaxError ("runner crashed").
- Swift emits the same text but has no `--` operator → loud build error
  `cannot find operator '--' in scope`. Anchor: the float Neg case in
  `sudoc/crates/backend_swift/src/lib.rs`. Same missing-paren class as
  the JS `!` bug generalized in backend-guide §4.10 — unary minus was
  missed.
- Int `-(-x)` is safe (js routes ints through `_rt.neg`);
  py/c/rs/zig/hs handle the float case correctly.
- Repro: scratchpad `negvar.sudo` / `negneg.sudo`.
- Direction: parenthesize the operand of unary minus whenever its
  rendering begins with `-` (or always). Add `-(-a)` / `-(-0.0)` float
  cases to floats.sudo.

### F5 — HIGH (zig): float literal arithmetic is evaluated at Zig
comptime_float (extended) precision — lockstep divergence on core IEEE
semantics

- `assert 0.1 + 0.2 == 0.30000000000000004` (true under IEEE binary64
  per-op rounding, spec §4.2) PASSES on py/c/js/swift/rs/hs and FAILS on
  zig: `0.3 != 0.30000000000000004`. The generated `(0.1 + 0.2)` is
  evaluated by Zig as comptime_float (exact) and only then coerced to
  f64 → exactly 0.3. Division is immune only because it routes through
  `rt.fdiv` (args coerce individually first).
- Anchor: `sudoc/crates/backend_zig/src/lib.rs:2178-2183` — float
  Add/Sub/Mul emit raw `({l} + {r})`; comparisons of comptime-known
  operands are affected the same way.
- Trigger breadth: any float expression whose operands are both
  comptime-known in the generated Zig. Runtime variables are unaffected —
  why the corpus stayed green.
- Repro: scratchpad `floatedge.sudo`; `sudoc test --target py --target
  zig floatedge.sudo` → DIVERGED on test_division_and_literal_fidelity.
- Direction: wrap emitted float literals in `@as(f64, …)` (cheapest), or
  route float +,-,* through rt helpers like fdiv, or fold float
  literal-literal arithmetic in the shared frontend (IEEE-correct Rust
  f64) so all backends receive the folded value. Add the 0.1+0.2 probe
  to floats.sudo.

### F6 — MEDIUM (zig): any sudo function with an unused parameter fails
to BUILD — `error: unused function parameter`

- Valid sudo (`func ignore_arg(x: int) -> int` / `return 42`) → Zig 0.16
  hard compile error on the generated fn. The backend discards its own
  unused `_sudo_ret_alloc` (`_ = _sudo_ret_alloc;`, lib.rs:1211-1213) but
  never discards unused USER params. Verified: `sudoc test --target zig`
  → "building for zig failed: unused function parameter".
- Loud, but breaks real programs (any callback-shaped function that
  ignores an argument). Anchor: `sudoc/crates/backend_zig/src/lib.rs:
  1143-1217` (emit_func has no param-use analysis or discards).
- Direction: emit `_ = x;` for params unused in the body (mirroring the
  `_ = &var` pattern). Add a conformance function with an unused param.

### F7 — MEDIUM (zig): two sequential `for i = …` loops in one scope —
`error: redeclaration of local variable 'i'`

- ForRange emission declares `var {var}: i64` (plus `_sudo_lo/hi/done`
  temps) at the CURRENT scope rather than inside a dedicated block
  (`backend_zig/src/lib.rs:1379-1407`), so §5.3's "loop variable is
  freshly scoped to the loop" breaks: two `for i = 1 to 3` loops in the
  same function fail to build on zig. Verified (`tworange.sudo`). Loud
  failure on an extremely common pattern (reusing `i`).
- Direction: wrap each ForRange in its own `{ … }` block, or uniquify
  the emitted loop variable. (For-in loops are immune — Zig's `for`
  capture is block-scoped.)

### F8 — MEDIUM (boundary/#17): nested-Option ambiguity is enforced only
for top-level RETURN types — params and record/enum fields slip through

- Spec (lockstep.md §5.1): "`Option<Option<T>>` in an export signature is
  a compile error — the collapse would be ambiguous." Enforcement is only
  `ret_has_nested_option` on the return type
  (`sudoc/crates/types/src/lib.rs:748-758`, applied ~:826). Two verified
  holes:
  1. **Param position**: `export func take_oo(x: Option<Option<int>>) ->
     int` compiles, and the generated JS in-conversion is nonsense — the
     same `x` null-tested twice:
     `((x)===null ? NONE : Some(((x)===null ? NONE : Some(host_int(x)))))`
     — `Some(None)` is unconstructible from the host.
  2. **Record/enum fields (new exposure since #17 Stage C)**: a field
     `weird: Option<Option<int>>` crosses the JS boundary and both
     `Odd(None)` and `Odd(Some(None))` serialize to `{"weird":null}` —
     two distinct sudo values, identical host value (verified via node).
     `ret_has_nested_option` has no record/enum descent (`_ => false`)
     while the #17 adapter now faithfully converts fields.
- Repros: scratchpad `boundary/bopt.sudo`, `boundary/blib.sudo`.
- Direction: extend the nested-Option check to param types and give it
  record/enum descent with the same visited-set guard as
  `boundary_contains_func`.

### F9 — MEDIUM (stdlib/design): `import std.bigint` is unusable — every
function's signature mentions the module-local `BigInt` record

- The v1 rule "module-local records/enums cannot cross module boundaries"
  (spec §9) rejects every `bigint` function cross-module:
  `'bigint.big_from_text' uses module-local types in its signature and
  cannot be called across modules yet`. Only the `base` constant is
  importable. The module's stated purpose ("the escape hatch for
  pseudocode that outgrows 64-bit int") is not achievable via import —
  users must paste the source. A composition of two documented decisions
  whose product looks unintended: the flagship stdlib module cannot be
  used as a library.
- Repro: any file importing std.bigint and calling
  `bigint.big_from_text("1")` → check error.
- Direction: pull cross-module record support forward, or ship a
  text/int facade in std, or document the limitation loudly.
- Related LOW: `factorial(n)` silently returns 1 for `n < 0` while
  `big_pow` asserts `e >= 0` — asymmetric hardening
  (`stdlib/bigint.sudo:237`).

### F10 — MEDIUM (cli/types): one error per file, always

- `check_program_with` (`sudoc/crates/types/src/lib.rs:266-270`) wraps a
  single-error pipeline (`check_program_inner(...).map_err(|e| vec![e])`);
  every stage bails at the first error; `cli/src/main.rs:87-90` prints
  `es[0]`. A file with 3 independent type errors in 3 functions reports
  exactly one; the `Vec<TypeError>` API is a facade. A lex error later in
  the file is reported *instead of* an earlier parse error (lexing runs
  first), so the reported error isn't even first-in-source-order.
  `sudoc check a b` does visit both files; `sudoc build` stops at the
  first broken file.
- Direction: accumulate per-declaration errors in the checker (parse
  errors can stay fail-fast); or at minimum document the single-error
  behavior.

### F11 — MEDIUM (hs, performance/resource):

- **Quadratic append, practically a hang**: `backends/haskell/
  SudoRt.hs:243-245` `appendL xs v = (xs ++ [v], ())` is O(n) per append.
  1M appends into an inout list: py ~1s; hs killed after >95s CPU
  (quadratic time), harness reports "no result (runner crashed?)" — i.e.
  a de facto lockstep failure on a program the other targets run fine.
  Direction: Data.Sequence or a difference-list representation.
- **Correction (2026-08-07):** the citation above was stale (function
  moved to SudoRt.hs:243-245); the quoted code and measurement text are
  unchanged and still accurate. Also, factually, the quadratic is not
  unique to `append` — the whole indexed-op family in SudoRt.hs is O(n):
  `at` (:233-236, `!!` plus a length recompute), `putL` (:238-241,
  `take`/`++`/`drop`), `popL` (:247-251, `length` + `take` + `last`),
  `swapL` (:268-277, two `putL` passes). The "only bites million-element
  inout appends" framing above understated the scope. (Factual note
  only — no fix direction implied beyond what is already stated.)
- **No RTS stack bound in the recipe**: GHC's default max stack is ~80%
  of RAM, so runaway recursion thrashes toward OOM where other targets
  trap at MB scale. With `+RTS -K8m` the runner's catch
  (SudoRt.hs:453-469) maps AsyncException StackOverflow → the trap kind
  correctly. Direction: add `-with-rtsopts=-K8m` (or similar) to the
  build line in `backends/haskell/hs.sudoc-backend.json`.
- Sanctioned-divergence note: py traps StackOverflow at ~100-200k depth
  where hs/others pass — that direction is the lockstep.md §3 carve-out,
  not a bug.

### F12 — LOW-MEDIUM (boundary/#17): `Result` in a param makes an export
silently vanish from the JS API file

- `export func take_result(x: Result<int, int>) -> int` compiles
  cleanly but is simply absent from the generated `.mjs`
  (`func_adaptable` → skip; anchor `backend_js/src/boundary.rs:86-97,
  186`). #17 Stage B converted the func-field skip into a hard error on
  exactly the fail-loud principle; Result-in-param kept the silent skip.
  A host consumer discovers the missing export at import/runtime.
- Verified: scratchpad `boundary/bres.sudo` — API contains only
  `give_result`.
- Direction: checker-reject Result-in-param on exports (it's spec'd
  out-only), or emit a loud build diagnostic.

### F13 — LOW (cli): assorted hardening (each verified)

1. Parser/checker native stack overflow on deep expressions (~5,000
   nested parens → SIGABRT exit 134) instead of a depth-limit
   diagnostic.
2. The entry path's extension AND existence are ignored: `sudoc check
   good.txt` with only `good.sudo` present reports `good.txt: ok`
   (`types/src/lib.rs:275-283` resolves dir + stem, never opens the
   typed path).
3. Unknown flags become file/module names (catch-all `f => files.push`
   at `cli/src/main.rs:204, 276, 478, 624`) — typos degrade into
   wrong-category errors and wrong exit codes.
4. Duplicate `--target py` → self-contradictory "unknown target 'py'
   (available: py, …)" (`take_by_name` removes on first use,
   main.rs:175-178).
5. `sudoc check` with zero files exits 0 silently (build/test guard;
   check doesn't).
6. Filename printed twice in parse/lex/IO errors (embedded in msg at
   `types/src/lib.rs:410,416-420`, prefixed again at main.rs:89).
7. Module names are never validated as identifiers: `verify-hs.sudo`
   compiles for most targets but explodes inside the Haskell backend as
   a GHC parse error (`import T_Verify-hs`). Spec §1 says module names
   are identifiers; nothing enforces it.
8. External-backend discovery is cwd-relative, so lockstep coverage
   silently varies by directory: the same `sudoc test` is 7 targets from
   the repo root, 6 anywhere else (hs quietly absent — one probing lane
   genuinely concluded "there is no 7th target"). Consider printing the
   registered target list on every lockstep run.

Clean under CLI probing: circular/self imports, std-shadowing rules, -I
precedence order as documented, missing-value flags, malformed external
manifests, non-UTF8/BOM/empty inputs, paths with spaces, SIGPIPE, exit
codes otherwise.

### F14 — LOW (stdlib/regex, documentation): `a{,3}` silently a literal

- Python parses `{,n}` as `{0,n}`; sudo treats `a{,3}` as the 4-char
  literal (verified: matches input `a{,3}`, not `aa`). Defensible under
  the documented brace grammar (`{m}`/`{m,}`/`{m,n}`), and every OTHER
  malformed brace treats-as-literal exactly like python — `{,n}` is the
  sole shape where the two diverge. Worth one header sentence in
  `stdlib/regex.sudo`.

---

## Clean sweeps (what was hunted and NOT found)

- **BigInt adversarial fuzz (prior item 3): CLEAN.** ~500
  oracle-generated cases (~1,900 asserts) vs Python int: add/sub/mul/
  neg/abs (0-200 digits, all sign combos, limb-base boundaries base^k±1,
  carry/borrow chains 10^k±1), big_cmp both orders, big_pow (0^0=1,
  negative bases, e≤201), big_divmod_small over its legal domain (+
  documented AssertFailed traps outside a>=0, 1<=d<base), from_text junk
  rejection + to_text canonicalization, big_to_int at exact i64 edges,
  factorial vs math.factorial — all green on py and under lockstep. The
  two overflow-sensitive spots (mul inner accumulator ≤ 10^18−1 < 2^63;
  to_int accumulation range-checked first) hand-audited sound. Probes:
  scratchpad `bigint-fuzz/`.
- **Regex groups + matcher perf (fresh item 8): CLEAN.** 2,800 fuzzed
  (pattern, input) cases + 72 adversarial hand cases vs Python `re`, 250
  glob cases vs fnmatch — zero disagreements; full 7-target lockstep on
  ~530 asserts agrees. `\(`/`\)` literals, nested/quantified/empty
  groups (`(a*)*`, `()*`, `(a|)b`), pipe-nesting, `[a|b]` all correct.
  Complexity: py scaling ratios 4.5x/4.1x/3.9x per input doubling =
  clean quadratic (the inout fix works; cubic would be 8x), c/js/zig
  flat; pathological `(a+)+b`-style inputs LINEAR in input length; no
  hangs (python's own backtracking `re` hung on a generated pattern
  sudo's Pike VM dispatches instantly). Probes: scratchpad `regex-fuzz/`.
- **Float edges (prior item 1): one divergence (filed as F5), the rest
  conform** across all 7 targets: sqrt edges (bit-exact sqrt(2.0),
  sqrt(-1)→NaN, sqrt(-0.0)→-0.0 with sign check, sqrt(inf)),
  float(int) nearest-even above 2^53, int(float(i64::MAX)) trapping
  after rounding, signed-zero arithmetic signs, inf−inf/inf·0/inf÷inf,
  sort placement of -0.0 vs 0.0 and NaN-last, round ties at 2^52−0.5 and
  2^51+0.5, denormal survival to 2^-1074 with exact round-trip, 2^53
  absorption. Existing floats.sudo coverage is strong; worth
  upstreaming: literal-arithmetic fidelity (F5 repro), sqrt (absent
  today), denormals, double-unary-minus (F4). Probe: scratchpad
  `floatedge.sudo`.
- **C sanitizers on current generated C (prior item 2): one finding
  (F3), the rest clean.** 17 targets (9 examples + pitfalls + 4 stdlib
  modules + infinite-craft kernel + 2 custom trap/copy stress probes) ×
  3 configs (ASan+UBSan norecover; UBSan+integer; plain + macOS `leaks
  --atExit`): zero sanitizer reports beyond F3; **0 leaked bytes in all
  17**, including trap-heavy paths — the longjmp arena reclamation holds.
  `-fsanitize=integer` hits investigated and benign (MurmurHash-style
  mixing; downto-loop `+%`-style wrap guarded by the exit flag).
  Valgrind unavailable on macOS arm64 (not attempted).
- **Zig memory overhaul beyond F1/F6/F7: holds up.** Composite stress
  (map-of-records-of-lists churned in nested loops, Option inout
  whole-value replacement in loops — the a0c2d18 fix, record-of-lists
  from hot loops, list-of-records across iterations) green in
  ReleaseSafe AND under the Debug DebugAllocator oracle; scalar and
  composite field tuple-swaps green across all 7; infinite-craft kernel
  18/18 in Debug (DebugAllocator) and ReleaseSafe. The tuple-assign
  inout path is safe in practice because the frontend hoists to temps +
  Var-assigns (verified in generated code).
- **#17 boundary beyond F8/F12: holds up.** Record/enum text fields map
  to host strings and round-trip (including `Option<text>` fields and
  enum payloads); recursive enums in export signatures terminate in both
  checker and adapter and round-trip through node; func-typed fields are
  hard-rejected including buried ones; multi-module JS emission adapts
  an imported module's record exports correctly.
- **Haskell deep-force under exotic shapes (prior item 4): no forcing
  stack overflows found** — 100k-node recursive enum equality and
  200k/2M-deep recursion fine in hs (py is the target that traps
  StackOverflow first, which is the sanctioned §3 divergence).
  `expect_trap StackOverflow` is deliberately rejected by the checker
  with a clear message (flakiness rationale) — judged correct, not a
  finding. The laziness findings are F2; resource findings F11.

## Coverage checklist (from the audit brief)

- [x] Float-edge probes vs floats.sudo coverage → F4, F5, clean sweep
- [x] C sanitizers (ASan+UBSan+leaks) on current generated C incl.
      kernel → F3, clean sweep
- [x] BigInt adversarial fuzz vs Python oracle → clean (F9 design note)
- [x] Haskell deep-force + StackOverflow catchability → F2, F11
- [x] CLI error reporting + arg hardening → F10, F13
- [x] Zig memory overhaul escape boundaries → F1 (+F6, F7 build bugs)
- [x] #17 boundary layer → F8, F12
- [x] regex groups + matcher perf → clean (F14 doc note)
- [x] Zig abort-path sanity glance (moot item) → kernel Debug oracle
      18/18, nothing further
