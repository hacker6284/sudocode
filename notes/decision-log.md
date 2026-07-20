# Autonomous decision log

Decisions made while Zach was away, per his instruction: "make your best
effort decision and write it down for me later." Newest last.

## 2026-07-19: Round-2 lane routing
All three remaining backends (Rust, Swift, Zig) go to the default grok lane,
no cross-vendor racing. Rationale: the conformance corpus is a strong
mechanical oracle (racing buys redundant confidence we already get from
lockstep verification), and the architect reviews every diff anyway. If any
backend fails two attempts, that specific one escalates.

## 2026-07-19: Zig toolchain + version pinning
Installed Zig 0.16.0 via Homebrew. Probe confirmed checked i64 arithmetic
returns catchable `error.Overflow` and IEEE NaN semantics hold. The 0.16
stdlib has renamed/reworked APIs vs. what models know from training
(GeneralPurposeAllocator, std.io); the Zig backend spec explicitly instructs
the implementer to probe installed APIs first and compile early.

## 2026-07-19: Rust backend design (pre-made for the spec)
- Traps: panic with a SudoTrap payload + `catch_unwind` in the test runner
  and expect_trap. Result-threading rejected: it would infect every
  generated signature.
- Memory: no manual frees — Drop is the arena. Panics unwind Drop, so
  trap-time leak-freedom comes for free (C needed an intrusive list for
  this).
- Value semantics: clone at aliasing store points (Python's rule);
  `&mut` params for inout — the native writeback.
- Derives: Clone + PartialEq always; Eq + Hash only for hashable types
  (f64 fields block those derives — backend replicates the checker's
  hashability walk).
- Float sort: custom comparator per spec §7, NOT `f64::total_cmp` — 
  totalOrder sorts negative NaNs first, which would diverge from py/c.
- Native HashMap blessed (randomized iteration is a feature per the
  guide amendment Zach prompted).
- Build: single `rustc` invocation over a module tree (`mod dep;`),
  no cargo; crate-level `#![allow(...)]` for generated-code lints.

## 2026-07-19: Swift backend design (pre-made for the spec)
- Traps: `throws` + do/catch. Swift's native overflow behavior is
  fatalError (uncatchable) — bare `+`/`-` on Int64 is FORBIDDEN in
  generated code; all int arithmetic goes through throwing helpers.
- Value semantics: native — structs/Array/Dictionary are COW values.
  Swift is the showcase target for this.
- Records/enums: Swift structs + enums with associated values;
  synthesized Equatable/Hashable align with sudo semantics (Double ==
  gives NaN != NaN even synthesized).
- `break` inside `switch` inside a loop: Swift switch captures `break`
  like C — use labeled loops (cheap and idiomatic in Swift).
- Multi-module: merged single file with `module__` prefixes (C's
  strategy) — Swift's one-module-per-compile namespace makes per-file
  emission collide.
- Closed ranges: `a...b` traps when a > b and has edge cases at
  Int64.max; loop lowering must guard (implementer verifies against the
  loops.sudo corpus edge test).

## 2026-07-19: Zig backend design (pre-made for the spec)
- Traps: error unions (`error.Overflow` etc. — kinds map to distinct
  errors, catchable natively); detail in a global buffer like C.
- Memory: per-test arena allocator, reset between tests — leak-free
  per test without C's intrusive tracking. v1 note like C had.
- Maps: std hash maps with custom hash/eql contexts for structural keys.
- Version: pinned to installed 0.16.0; probe-first instruction in spec.

## 2026-07-19: JS backend (round 1) — accepted on first attempt
Grok-lane implementation, verified independently: 9/9 conformance across
py/c/js, 34 lockstep modules, 0 clippy warnings, readable output (checked
partition by eye). Two bugs surfaced and were fixed in-flight (JS `!`
precedence; user `record BigInt` shadowing the host global) — both
generalized into backend-guide §4.10/§4.11. One authorized out-of-spec edit:
a harness test hardcoded the backend count; now derives from the registry.

## 2026-07-19: grok lane environment constraint
In this sandbox the grok CLI cannot execute mutating shell commands
(headless permission modes cancel them) — it writes files; the wrapping
agent runs cargo/node verification. This worked well and the doctrine
already demands independent verification; keeping the pattern for round 2.

## 2026-07-19: latent shadowing bugs in backend_py (deferred)
The §4.11 finding applies to Python too: generated code calls bare `len(x)`
and `list(x)`; a user `func len` or `record list` would shadow them. Legal
but unconventional sudo; deferred as a hardening task (route through _rt)
rather than blocking round 2. Not reachable from the current corpus.
