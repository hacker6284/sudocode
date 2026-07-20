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

## 2026-07-19: lane wrappers must run grok in the foreground
All three round-2 wrapper agents independently hit the same failure: launch
the grok CLI in the background, then stop to "await a notification" —
orphaning the process, since a stopped wrapper's untracked background
children never wake it. Redirected all three to plain blocking foreground
grok calls (long timeouts, sequential chunks, verification between calls).
Future delegation prompts should mandate foreground invocation upfront;
the JS round only avoided this by luck of its wrapper's choices.

## 2026-07-19: round-2 worktree pruning — coordination hazard
The Swift and Rust lanes' isolated worktrees were pruned mid-task
(environment churn), so both wrote directly into the MAIN checkout instead.
Consequences being managed carefully:
- backend_swift and backend_rs both landed uncommitted in main; both are
  registered in the shared Cargo.toml / harness lib.rs (non-atomic edits by
  two lanes, but current state has both present and consistent).
- The Rust agent is STILL LIVE editing backend_rs in this same checkout, so
  no workspace builds/commits until it finishes (would race its writes).
  Swift verification deferred until then — combined verify is cleaner anyway
  since both are already integrated.
- Swift lane reported a pre-existing format!-brace-escaping bug in
  backend_rs causing unfiltered conformance/test failures; the Rust agent
  was resumed specifically to resolve it. Will confirm on Rust completion.
- Zig lane is correctly isolated in worktree-agent-zig-backend.
Plan on Rust completion: verify swift+rs together (scoped + full), commit
both to main as one integrated round-2 landing, then handle Zig from its
worktree.

## 2026-07-19: round-2 Rust + Swift accepted (5 targets green)
Both verified independently by me: cargo test --workspace 0 failures, 0
clippy warnings, 9/9 conformance across py/c/js/swift/rs, generated
partition() readable in both. Committed as one integrated landing (worktree
pruning had already merged them into main). New guide land mines harvested:
§4.1 borrow-checker temporaries (Rust E0502 free-fn &mut aliasing — flagged
as likely relevant to Zig), §4.13 uncatchable traps (Rust/C StackOverflow,
Swift fatalError-on-overflow → all int math through throwing helpers).
Grok found+fixed two real bugs per lane under my verification (Rust: nested
format! brace-escaping + the E0502 aliasing; Swift: @main/main.swift
collision, closure-param over-shadowing, keyword over-escaping). Environment
friction (worktree pruning, grok acceptEdits no-op) cost most wall-clock but
did not affect deliverables.

## 2026-07-19: Zig lane retired from grok, rerouted to Claude subagent
The grok-implementer wrapper for Zig stalled four times on the
background-and-wait pattern, ignoring explicit foreground-only instructions
and the self-implement fallback — the wrapper simply would not stop
backgrounding. Per orchestration doctrine ("if a CLI lane is unavailable,
implement with a Claude subagent and state the downgrade plainly"), retired
the grok-Zig lane. Salvaged its worktree's backend_zig crate first
(2366-line emitter + 354-line runtime; compiles as Rust, unverified,
unregistered), stashed it to scratchpad, seeded a fresh worktree
(zig-finish, off current main so it has swift+rs+guide updates), and handed
the finish to a general-purpose Claude subagent that uses its own
Read/Write/Edit/Bash directly (no grok CLI → no backgrounding pathology).
Downgrade is stated: Zig is the one backend not built by the cross-vendor
grok lane. If it lands green it still validates the SDK (a non-me
implementer following only the guide); the guide harvest is unaffected.

## 2026-07-19: Zig 0.16 API findings salvaged from retired grok lane
Before retirement, the grok-Zig wrapper verified these 0.16 stdlib facts by
direct compilation (relayed to the finishing Claude subagent, destined for
notes/friction-zig.md as the version-pinning reference):
- GeneralPurposeAllocator -> std.heap.DebugAllocator; ArenaAllocator.init(
  page_allocator) + .reset(.retain_capacity).
- ArrayListUnmanaged/StringHashMapUnmanaged: `.empty` init, allocator per call.
- std.math.add/sub/mul/negate -> catchable error.Overflow; std.math.round
  already ties-away-from-zero.
- @divFloor/@mod panic uncatchably on divisor 0 -> guard explicitly first.
- @intFromFloat/@floatFromInt (renamed from pre-0.16); guard NaN/range.
- stdout: std.c.write(1,ptr,len) with -lc (no clean getStdOut in 0.16).
- break/continue in switch-in-loop targets the LOOP (opposite of C).
- Unmutated `var` is a HARD COMPILE ERROR in 0.16 -> emit const or `_ = &name;`.
- Build: zig build-exe {entry}.zig -femit-bin=sudo_tests -lc -O ReleaseSafe.
Also isolated the grok CLI headless write-permission recipe (bypassPermissions
+ --disallowed-tools on shell tools) — noted for future runs, lane already retired.
