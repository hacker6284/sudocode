# sudo / sudoc Red-Team Findings — 2026-07-22

All repros live under this directory (`adversary/`). Commands run from the repo root with
`SUDOC=sudoc/target/release/sudoc`. Spec citations: `spec/language.md` (L§), `spec/lockstep.md` (LS§).

## CRITICAL

### F1 — D: round(0.49999999999999994) splits backends 4 vs 3
File: min/d05_round_half_ulp.sudo. Cmd: `sudoc test`.
Output: DIVERGED — py/js/rs/hs: `trap AssertFailed — {"f": "1.0"} != {"f": "0.0"}`; c/swift/zig: pass.
Spec: L§4.3 round = IEEE-exact nearest, ties away from zero; the largest double < 0.5 must round to 0.0.
Why: py/js/rs/hs implement ties-away as trunc(x+copysign(0.5,x)), which double-rounds at the half-ulp edge.

### F2 — D: (-2^63) mod -1 — rs traps "Unknown", six targets return 0
File: min/d01_min_mod_minus1.sudo. Cmd: `sudoc test`.
Output: DIVERGED — rs: `trap Unknown`; py/c/js/swift/zig/hs: pass.
Spec: L§4.1 floor mod; only `mod 0` and `(-2^63)/-1` trap; result 0 fits. L§8: trap kinds are a closed set — `Unknown` is not one; Rust leaks a raw `i64::MIN % -1` panic.

### F3 — U: xs[0].append(v) / m[k].append(v) — zig backend PANICS sudoc; swift emits non-compiling code
File: min/u06_swift_mapelem_append.sudo.
swift: `error: cannot use mutating member on immutable value: 'mapAt'/'listAt' returns immutable value`.
zig (run with swift excluded): `thread 'main' panicked at crates/backend_zig/src/lib.rs:2406:5: stage 2: place Index as bare lvalue not yet implemented`.
py/c/js/rs/hs: all ok and agree.
Spec: L§7 mutating ops need a mutable path; a[i]/m[k] are assignable places (L§5.1); checker accepts.
Why: checked, 5-backend-agreed bread-and-butter code (adjacency lists) crashes the compiler.

### F4 — U: JS emits undeclared temp `_sudo_r` for `a[i] = f(...)` / `p.field = f(...)` with an inout call on the RHS
Files: min/u03_js_rhs_only.sudo (minimal), min/u03b_js_field_target.sudo, rt/r10_assign_order.sudo.
Output: `js  _sudo_r is not defined` (strict-mode ReferenceError). Generated (gen/js_r10/_r10_assign_order_impl.mjs):
`let _sudo_h0;` … `[_sudo_r, n] = bump(n);` — `_sudo_r` never declared.
Spec: L§5.2 inout calls allowed anywhere an expression can appear. Other six targets pass and agree the index target evaluates before the RHS (L§12).

### F5 — U: Haskell emits min-int literal unparenthesized in argument position
File: min/u02b_hs_min_literal_arg.sudo (`x = float(-9223372036854775808)`).
Generated: `case Rt.floatOfInt -9223372036854775808 of` → GHC parses as subtraction → build fails.
Spec: L§3 the min literal is legal directly after unary minus, including as a call argument. `abs(-5)` works — only the IntMin special path forgets parens.

### F6 — U (systemic): target-language reserved words not escaped — py, c, js, rs, zig break
Files: kw/kw_py.sudo, kw/kw_c.sudo, kw/kw_js.sudo, kw/kw_sw_rs_zig.sudo (all pass `check`).
py: generated `lambda = 1` → SyntaxError (runner crashed). c: `int64_t restrict = volatile;`.
js: runner crash (`let`/`const`/`function` vars). rs: `let mut impl: i64 = fn;`. zig: `pub fn guard(defer: i64)`.
hs and swift are ROBUST (kw_hs.sudo incl. lowercase record/enum names passes on hs; swift passes its keyword set).
Spec: L§1 these are legal sudo identifiers; LS§5.3 backend contract.

### F7 — U + silent wrong answer: `__` mangling collides with user identifiers
File: b3/mangle_collision.sudo — user `func pick__i64` + generic `pick<T>` at int (check: ok).
py: SILENTLY WRONG — `FAIL … py line 11: 1 != 1001` (generic call routed to the user function; later def wins).
c: `error: redefinition of 'pick__i64'`; swift/rs/hs/zig: redefinition errors; js: runner crash.
Spec: LS§7 mangling `sort__i64`-style; L§1 makes `pick__i64` a legal identifier; no reservation of `__` names.
Why: the Python variant is a wrong-answer bug with no diagnostic — closest thing to a soundness hole found.

### F8 — U: module-prefix mangling collision
Files: b3/mod/a.sudo (fn b_c), b3/mod/a_b.sudo (fn c), b3/mod/mainmod.sudo. Cmd: `sudoc test -I b3/mod b3/mod/mainmod.sudo`.
Output: C `error: redefinition of 'a_b_c'`.
Spec: LS§8 claims prefixing keeps names "collision-free"; underscore join is not.

### F9 — U: user `func abs` / `func min` — six targets agree (user fn wins), C build fails on libc collision
File: b6/shadow_builtin.sudo. C: `static declaration of 'abs' follows non-static declaration` (SDK _stdlib.h).
Spec: abs/min are not reserved (L§1); frontend resolves to user functions; py/js/swift/rs/zig/hs pass (`abs(-5)==95`).

### F10 — U: duplicate literal match arms (valid: "first matching case wins", L§6.3) break C
File: b3/match_dup.sudo (check: ok). C: `error: duplicate case value '1'` from switch lowering.

### F11 — U/A: recursive record passes `check`; C emits `struct R { R next; … }`
File: static/s16_recursive_record.sudo. check: ok; C: `error: field has incomplete type 'R'`.
Spec: L§6.2 sanctions recursive enums only; silent on records (type is uninhabitable). Check/codegen disagree; spec gap.

## MEDIUM

### F12 — C: qualified variant construction `A.Red` rejected; shared-name variants unconstructable
Files: b3/qualified_single.sudo → `unknown variable 'A'`; b3/ambiguous_unqualified.sudo → `variant 'Red' is ambiguous (A, B); qualify it`.
Spec: L§6.2 "may be qualified (Tree.Leaf) to disambiguate"; patterns accept qualification, expressions don't. The two diagnostics contradict each other.

### F13 — C: `expect_trap AssertFailed` (and StackOverflow) rejected
File: b3/et_assertfailed.sudo → "'AssertFailed' is not an expectable trap kind (one of: …7 kinds…)".
Spec: L§5.4/§8 place no restriction on KIND.

### F14 — C (minor): `while true` + internal return rejected as "not every path returns"
File: b5/while_true_return.sudo. Conservative per L§5.2 but rejects a common CLRS idiom.

## DOC / AMBIGUITY

### F15 — A: module constants restricted to "scalar constant expressions"
File: static/s18_const_mut.sudo (`base = [1, 2, 3]`) → rejected. L§5.1/L§10 promise `constant_expression`/`expr` with no scalar restriction.

### F16 — A: leading-zero int literals (`007`) accepted; L§3 silent. Backends canonicalize (no divergence today).

### F17 — A: duplicate tuple-assignment target `x, x = 1, 2` accepted; all targets agree x == 2; L§5.1 silent.

## Appendix — attacks correctly handled (negative results)

Frontend: inout aliasing f(x,x)/f(p.x,p.x)/f(p,p.x); inout of list element; loop-var assign & loop-var inout;
Map float key & Set of float-payload enum; function comparison; non-exhaustive int match; cases after `_`;
def-assignment via while-only/if-only paths; `0 - 9223372036854775808`; export Option<Option<int>> & export generic;
pattern arity; rvalue `m.keys().sort()`; `List<List<int>>.sort()`; 32-instantiation monomorph guard;
expect_trap non-final/outside-test/unknown-kind; `\u{D800}`, `\u{110000}`; CRLF source; `1_000`; `A()` empty payload;
duplicate test names; import cycle; `import std.sorting` vs file sorting.sudo; `for a, b in list`.

Runtime (all seven agree, ASan/UBSan clean where C is involved):
for-to/downto at ±2^63 bounds; `outer.append(outer[0])` & `insert(0, xs[last])` across realloc;
int(float) at exact ±2^63; products landing exactly on ±2^63; `-m`/`m-1` overflow traps at min;
floor div/mod at min/±3 (intermediate q*b overflow trap for naive helpers) and max mod -2;
text sort/mutate/astral/empty; index-target-before-RHS evaluation order (6 targets; F4 is the JS outlier);
reads `a[bump(n)]` in expressions/conditions on JS; short-circuit skips trapping operand;
snapshot iteration List/Set/Map; filled(3,[1]) no aliasing; grid[0][1]=5 nested writes; values() copies;
tuple/record/enum/text structural keys; p.x,p.y swap; round/floor/ceil away-from-zero non-fixed cases;
sqrt(-0.0); min/max(-0.0,-0.0); float(2^53±1) nearest-even; nested Option internally; 4000-deep parens;
builtin shadowing semantics (6 targets); `007` consistent; `x,x=1,2` consistent; `for ch in "abc"`; return-in-test.

Total distinct attack programs: 63.

---

## Triage & rulings (coordinator, 2026-07-22)

Source: a Fable red-team lane, findings-only. Repros archived from the
session scratchpad. Pattern: every bug is in BACKEND CODEGEN, not the
frontend/type-system — the 63-attack negative-results appendix above is
strong evidence the semantic core is sound.

Owner rulings this session:
- F11 recursive record: REJECT at check-time with a clear diagnostic
  (the useful forms — Option<Self>, List<Self> — already work via
  indirection; only the uninhabitable direct form is the bug). Support
  via auto-boxing is backlog.
- F13 expect_trap kinds: allow ALL defined kinds EXCEPT StackOverflow
  (non-deterministic/undetectable). Unblocks the bigint pow/divmod
  trap tests the float lane couldn't write.
- F15 module constants: EXPAND the impl to fold container/composite
  const expressions (spec was right; impl was too strict).
- F16 leading zeros / F17 duplicate tuple-assign targets: ACCEPT both
  (no octal in sudo → 007 is unambiguously 7; last-write-wins is right).
  Pin in spec §3/§5.1; no code change.

Fix waves:
- 1a (dispatched): round-half-away correctness (F1 + rs/js sign bug) +
  F2 (Rust mod INT_MIN/-1 = 0, and NEVER emit "Unknown" trap kind).
- 1b: codegen crashes/build-failures — F3 (zig backend panic), F4 (js
  inout-hoist temp missing let), F5 (hs min-int literal parens), F10
  (C duplicate case label).
- 1c: identifier hygiene — F6 (target-keyword escaping), F8 (module
  symbol-join collision), F9 (libc name collision in C). F7 (mangling
  collides with user `__` names → Python SILENT WRONG ANSWER) is the
  most serious find; its fix (collision-proof mangling scheme) is a
  design choice held for owner input.
- 2 (rulings→code): F11 reject, F13 kinds, F15 container consts, F12
  qualified variant construction, F14 while-true+return completeness,
  F16/F17 spec pins.
