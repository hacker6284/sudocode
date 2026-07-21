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

## 2026-07-19: Zig expect_trap bug — caught by independent full-suite verify
Zig subagent reported six-target conformance green; MY independent
`cargo test --workspace` found 2 harness lockstep failures it missed. Root
cause: its expect_trap lowered the block into a nested `struct { fn run }`,
but Zig nested fns have NO closure over outer locals — any trap block
referencing an enclosing variable fails to compile. Conformance passed only
because traps.sudo self-contains its locals; the two harness tests don't.
Textbook "reports are claims, verify everything." Sent back with diagnosis +
fix direction (inline labeled block, or pass outer locals as explicit
params). Not merging until cargo test --workspace is 0 failures.

## 2026-07-19: ALL SIX TARGETS GREEN — round 2 complete
Zig merged after the subagent fixed expect_trap (inline labeled block — Zig
nested fns can't capture; harvested to guide §4.12b). Final independent
verify on main: cargo test --workspace 0 failures, 0 clippy warnings,
`sudoc conformance` 9/9 across py, c, js, swift, rs, zig. Worktrees pruned.

Scorecard: JS accepted first try (grok). Rust + Swift accepted after
grok found+fixed its own bugs under my verification (grok lane). Zig: grok
lane retired for the backgrounding pathology, finished by a Claude subagent
(stated downgrade), and its self-reported-green was WRONG — my full-suite
verify caught an expect_trap capture bug conformance missed; fixed on
re-spec. Every backend's friction became guide improvements (§4.10-4.14).
The SDK held up: six independent-ish implementations, one small trait, one
corpus as the contract, zero SDK changes needed to add a backend.

## 2026-07-20: two process failures Zach called out, and remedies
1. Permission-mode knowledge not propagated: the JS lane's report documented
   the acceptEdits no-op + auto workaround; round-2 specs carried the
   Execute limitation but not the flag guidance, so Rust and Swift both
   re-derived it. NEW RULE: friction logs have TWO consumers — sudo-facing
   lessons go to the backend guide, ops-facing lessons go into the next
   delegation spec verbatim. The full working grok headless recipe (from
   the retired Zig wrapper): --permission-mode bypassPermissions plus
   --disallowed-tools run_terminal_command,Shell,AwaitShell,Await — grants
   writes while structurally blocking mutating shell.
2. Rust+Swift provenance gap: worktree pruning put both lanes in one
   checkout concurrently; committed as one entangled landing, so mechanical
   per-lane isolation rests on self-reports. Clarification of what was/
   wasn't lost: backend-to-backend implementation independence was never
   clean-room (all lanes read reference backends by design); runtime
   diversity is proven by the divergence machinery. What's missing is
   mechanical proof neither lane wrote into the other's crate. Remedy in
   flight: forensic transcript audit extracting every Write/Edit target per
   lane, cross-checked against commit 0b7797c. Fallback if the audit can't
   prove isolation: rerun both lanes in clean worktrees with the fences
   (foreground calls, no shared-file edits) — Zach's call.

## 2026-07-20: grok CAN execute commands headlessly — verified recipe
Answering Zach's question with a live probe, not agent hearsay:
`grok -p "<task>" --permission-mode auto --allow "Bash(echo *)"` executed
the command and returned its output. So the self-verifying-lane setup for
future rounds is: `--permission-mode auto` plus scoped allow rules in
Claude-Code-compatible grammar, e.g.
  --allow "Bash(cargo *)" --allow "Bash(zig *)" --allow "Bash(node *)"
  --allow "Bash(swiftc *)" --allow "Bash(./sudo_tests*)"
No blanket bypass needed; --deny available for carve-outs; --sandbox
profiles exist for fs/network confinement; --check adds a headless
self-verification loop; grok also has NATIVE --worktree isolation.
Root cause of all the round-2 permission pain: nobody — including the
architect — ever ran `grok --help`. The wrappers followed the routing
skill's prescribed flag (acceptEdits, which silently no-ops headless) and
iterated on failure modes instead of reading the manual. Process rule:
before delegating through any CLI lane, read its --help and probe the
permission path with a one-liner first.

## 2026-07-20: forensic audit verdict — write-isolation PROVEN
The provenance gap is closed with primary evidence, not self-reports: every
grok session log survives under ~/.grok/sessions/ with per-call permission
decisions and timestamps (the writer's own record, distinguishing executed
writes from cancelled attempts). Audited all 13 sessions across both lanes:
- ZERO writes by either lane into the other's crate, any other backend, the
  SDK, spec/, or conformance/. Every completed write classifies as
  own-crate / the sanctioned shared-3 registry lines / own friction log /
  transient probe (deleted).
- Architect spot-checks on raw logs confirm: "backend_rs" appears ZERO times
  in the Swift lane's session; the Rust lane's 10 "backend_swift" mentions
  are all additive shared-registry edits explicitly preserving Swift's
  entries (sequential edits ~04:35 vs ~04:50 UTC, no clobber).
- The commit's two unattributed files resolved: Cargo.lock (cargo-generated
  by wrapper builds) and notes/decision-log.md (the architect's own entry,
  swept into the commit by `git add notes/` — confirmed from this session).
Remaining honest limits: READ isolation is not provable (Rust ran one
read-only find over backend_swift) and was never a design property — all
lanes were instructed to read reference backends. Conclusion: the committed
Rust and Swift crates are mechanically attributable to their lanes; no
rerun warranted.

## 2026-07-20 — Public release: docs, CI, push

- CI shape: macOS primary job (only free runner with all six toolchains —
  clang, python3, node, swiftc preinstalled; Zig via mlugg/setup-zig@v2
  pinned 0.16.0) running the full gauntlet: clippy -D warnings, workspace
  tests, release build, 6-target conformance, examples+stdlib lockstep,
  rustdoc -D warnings. Linux job is a 5-target conformance sanity pass
  (no Swift on ubuntu runners) rather than a matrix — one honest full job
  beats a matrix of partial ones.
- rustdoc promoted to a gate (RUSTDOCFLAGS=-D warnings) after finding 8
  broken-doc sites locally (`List<int>` parsed as HTML tags, `[DIR]`/`[0]`
  as intra-doc links). If docs are a product surface, they get a CI gate
  like everything else.
- Pushed main (aef2c72) to github.com/hacker6284/sudocode after local
  verification of the exact CI steps: workspace 0 failures, 9/9
  conformance, all examples+stdlib lockstep-green.

## 2026-07-20 — External backend protocol (v1) committed

- Direction (Zach): sudo must not be "Rust under the hood" — protocol
  strong enough that in-tree backends could use it. Sharpened to:
  wire format is the single DATA contract (wire-trip CI: serialize→
  deserialize→emit must be byte-identical for all six in-tree backends),
  but the six stay in-process Rust to preserve single-binary UX.
  "All backends out of process" deferred until the protocol earns it.
- Flagship external backend: Haskell, chosen (Zach, from recommendation)
  as maximally hard "the old way" — pure target forces loops+inout →
  tail recursion (ST fallback allowed), best-in-class authoring fit,
  and it stress-tests that the IR isn't imperative-chauvinist.
- Advisor consult (commitment boundary) confirmed architecture; caught:
  (1) IrParam::boundary/ret_boundary leak surface TypeExpr over the
  wire → closed BoundaryTy in schema v1; (2) float encoding must be
  pinned or wire-trip flakes; (3) Backend::name() &'static str forces
  leaks for manifest-named backends → &str.
- My addition: i64 crosses the wire as decimal strings (JSON numbers
  corrupt beyond 2^53 in f64-based parsers); text scalars stay plain
  numbers (bounded by 0x10FFFF).
- Schema generated from Rust types (schemars) and committed as golden
  with drift check — hand-written schema would be a second source of
  truth. Exact-version match, deny-unknown-fields; no capability
  negotiation in v1. One process per emit; recipe templates live in
  the manifest ({entry} substitution) so emit is a single round trip.
- spec/protocol.md is normative; tasks: wire layer → ExternalBackend
  adapter → Haskell backend (conformance-green = acceptance).

## 2026-07-20 — Haskell lane routed grok, not codex (substitution notice)

Intended route for the Haskell flagship was the cross-vendor codex lane
(most correctness-critical task; wanted a third model family). The codex
CLI is not installed on this machine — lane returned `unavailable`
without attempting work, per protocol. Re-routed the identical spec to
grok (which built 4 of the 6 in-tree backends), stated here rather than
silently absorbed. If codex gets installed later, a worthwhile follow-up
is racing it on a second implementation of one conformance module's
emitter as an independent check. Verification posture unchanged:
architect re-runs the full acceptance gauntlet regardless of lane.

## 2026-07-20 — Haskell external backend accepted (protocol proven)

- backends/haskell/ (Emit.hs + SudoRt.hs + SudoJson.hs + manifest, GHC
  boot libraries only — hand-rolled JSON, no aeson/cabal) is
  conformance-green: 9/9 across py, c, js, swift, rs, zig, hs, and
  33/33 on examples+stdlib lockstep, including bigint. The protocol is
  now proven from the outside: a conformant backend whose code never
  links sudoc_ir.
- Purity strategy that shipped: two-mode compilation (expr-mode for
  straight-line bodies, loop-mode via a Flow(Cont/Brk/Ret) sum threaded
  through local recursive `go` functions over exactly the mutated
  variable set); inout via writeback-by-return; strict rebinding via
  case-bang (Haskell `let` is recursive — `let !n = n + 1` is a black
  hole, the lane's biggest find).
- Held the readability bar: sent back for a peephole pass (collapse
  forced-bind double-hops, no re-casing bare vars, tail-identity
  elision, drop inferable literal annotations, all gated on a
  free-variable check that the lane discovered was necessary —
  collapsing genuine value copies broke value semantics until gated).
  quicksort now emits as a single call expression + one readable
  recursive partition.
- One accepted iteration on my spec: --target cannot name external
  backends (they register via --external only) — logged as CLI polish,
  not blocking.
- Verification: every acceptance number above re-run by me from the
  working tree before commit, per standing rule.

## 2026-07-20 — Discovery, two-paths policy, and the hosting audit

- Zach pressed the "privileged language" concern: if external backends
  are worse, Haskell is second-class; if not, why not unify all-external?
  Resolution (agreed): the dilemma conflates target language with emitter
  hosting. Capability equality is mechanical (wire-trip); remaining
  differences are ergonomic and attach to the emitter's implementation
  language and maintainer, not the target. In-tree backends are
  REFERENCE IMPLEMENTATIONS (Zach's framing) — the oracles conformance
  diffs against — not incumbents; independent external implementations
  of covered languages can lockstep-diff against them under distinct
  names ("no privileged implementer").
- Greenfield hosting audit (sunk cost excluded, per Zach): C in-tree
  (emitter needs a compiler language), Swift in-tree (emitter-in-Swift
  would kill Linux emission), Zig in-tree (pre-1.0 churn would double
  the migration surface — emitter + generated dialect; time-indexed
  verdict, decays to tiebreak post-1.0), Python/JS in-tree by
  maintainership tiebreak only (honest migration candidates if outside
  maintainers appear), Haskell external on native-taste asymmetry.
  Codified as backend-guide §0 rubric + protocol.md §6 policy.
- Discovery shipped: backends/*/*.sudoc-backend.json auto-registers,
  --target resolves externals, name collisions fatal, malformed
  manifests hard-error; --external is now the escape hatch. Plain
  `sudoc conformance` = seven targets. macOS CI installs GHC (full
  seven-target gate on both platforms).
- Grok permission churn: NOT solved by allowlist breadth — lane found
  `--permission-mode auto` has a nondeterministic "confirmation floor"
  on the shell tool that no --allow fixes (even bare Bash catch-all).
  Reliable pattern (notes/lane-recipe.md): file-authoring lane with
  shell tool stripped + wrapper runs verification; shell lane only
  under close supervision. Container+yolo remains the fallback if this
  still churns; bare yolo on host stays forbidden.

## 2026-07-20 — Pilot: kernel ported, divergences ruled, 3 compiler bugs found

- infinite-craft kernel in sudo: 27 tests green across all seven targets;
  six documented divergences between the Python and JS originals, each
  isolated in a _js sibling. Zach ruled all six for the Python behavior
  (fnmatch classes, shared-matcher ^ filter, unbounded BFS, no flag
  promotion, exact+title() lookup, closure export) — wired paths stood.
- JS boundary adapter shipped (lockstep §5.4) with two spec rules learned
  by building it: Result is out-only; text intent doesn't survive named
  types (records kept out of export signatures; proper fix = per-field
  boundary intent on record declarations, task #17).
- Dogfooding found three real backend bugs, routed around at source level
  by the port and now to be fixed with regression coverage: Swift
  skip-only match arms fail to compile; Zig hard-errors on unused match
  arm binders (no per-field wildcard); Rust drops &mut on cross-module
  inout calls (emitter resolves callees only in the current module).
  Cross-module emission is under-covered by the conformance corpus —
  the fix ships with a multi-module conformance module.

## 2026-07-21 — Re-port: pilot was built on a five-release-stale base

- Remote infinite-craft-cli main had moved c895fa3 → v1.4.2 (27 commits,
  ~15k insertions) while the pilot ran: matching subsystem extended
  (scan budgets, query-length caps, regex classification, parse
  filters), trainer churned ~1.2k lines, storage semantics adjusted,
  plus a hand-maintained py-vs-js parity test ("keep in sync when
  changing either side" — the manual version of what we automate).
- The sudo-kernel branch merges only against the past; local main was
  quietly stale. Unwound without touching Zach's WIP (branch -f, no
  hard reset; WIP later stashed, labeled, recoverable).
- STANDING RULE: before extracting logic from any external repo,
  fetch and verify the local base equals remote HEAD, and record the
  base commit in the port's DIVERGENCES/report. Staleness cost us a
  full port iteration.
- Decision (Zach): re-port against v1.4.2 on sudo-kernel-v2, carrying
  infrastructure (generate script, parity harness, workflows, vendored
  stdlib) and re-extracting the kernel; v1.4.2's manual parity suite
  becomes spec input for the new divergence audit.

## 2026-07-21 — std.* embedded stdlib + -I; vendoring eliminated

- Zach flagged sudo-source duplication (vendored regex/strings in the
  icc kernel) as unacceptable. Decision: stdlib ships EMBEDDED in the
  sudoc binary (import std.regex — Go-style, versioned with the
  toolchain, zero config), plus repeatable -I search paths for
  non-stdlib sharing. Full package system explicitly rejected as
  premature (no ecosystem yet); per-language published packages of
  generated stdlib noted as a later, demand-driven distribution
  channel for non-sudo consumers.
- Key invariant: std-imported and file-imported programs generate
  byte-identical output (CI-tested), which made the downstream
  migration a two-line import change + three file deletions with
  provably zero behavior change.
- Also this session: regex.sudo grew alternation (kernel exceeds
  upstream) and escapes (ASCII predefined classes; \b tracked); JS
  backend emits readable text literals (found by a downstream static
  test unable to grep generated messages); Zig gained the nested
  else-if scoping fix, Option/Result binder guards, and shared
  cross-module monomorphized types.

## 2026-07-21 — sudoc v0.1.0 released; consumer bazelified and pinned

- sudocode's first release: v0.1.0, per-platform binaries (macOS arm64,
  Linux x64/arm64) with sha256s, built by a tag-gated workflow with a
  dispatch rehearsal mode. "Try sudocode" no longer requires cargo.
- infinite-craft-cli consolidated on Bazel (Zach's call: one entry
  point per test): parity harness is //tests/parity:parity_test (one
  comparator, bazel + bare-pytest paths); sudo.yml deduped to
  toolchain integration only; all five workflows shed Rust builds —
  sudoc acquired by scripts/sudoc-bin.sh (SUDOC_BIN override → cached
  → checksum-verified download of the PINNED release). Bazel is fully
  self-contained via platform-selected http_file binaries + genrules:
  fresh checkout, zero generated files, bazel test green, no pre-step.
- Toolchain upgrades are now an explicit reviewable bump of
  scripts/sudoc-version.txt — the floating-main coupling is dead.
- Release-gating lesson compounded: publishes (PyPI, Pages) fire only
  on v* tags; dry-run workflows rehearse every step but the uploads.
  The pytest-not-a-dep failure was masked locally by a pre-existing
  .venv and remotely by a stale branch filter — declared deps and
  pre-merge CI exercise are both now structural, not habits.
