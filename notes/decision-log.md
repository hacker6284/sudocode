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

## 2026-07-22 — rules_sudo shipped and consumed

- Zach's call, small scope held: toolchain extension (pinned
  version→sha256 manifest, platform hub alias, local_binary dev
  override), sudo_library/SudoInfo, sudo_py_library (tree artifact →
  PyInfo; key finding: imports entries must be workspace-name-prefixed
  or _bazel_site_init silently misses them), sudo_js_library,
  sudo_lockstep_test. e2e module is the acceptance gate (clean-expunge
  green against real release binaries). Release workflow attaches
  rules_sudo-<tag>.tar.gz; v0.1.0 got the tarball backfilled.
- infinite-craft-cli is the first production consumer: http_file/
  select/genrule plumbing deleted, kernel lockstep now a bazel test,
  tree artifacts land at package-nested _sudo paths with ZERO source
  changes. Groups (#24) and \b (#23) remain the regex backlog [both since shipped
  — \b at the R5 entry below; groups landed, see stdlib/regex.sudo]; BCR
  publication deferred until external demand.

## 2026-07-22 — Intent grilling: seven query-semantics rulings (Zach)

Motivation: the "too complex" screenshot showed our tests derive
expectations from code (oracles, ports, parity), never from owner
intent. Grilled query-by-query; answers are now the spec:
R1 `!`+`^` prefixes COMBINE, either order (behavior change, kernel).
R2 Mid-pattern ^/$ are real anchors — /a^b/ matches nothing, like
   every mainstream engine (engine change; replaces edge-only-literal).
R3 Bare pipe (fire|water) stays a silent literal substring — no hint.
R4 Delimiter edges pinned as-is: /fire and fire/ are substrings,
   // matches everything.
R5 \b/\B get IMPLEMENTED (task #23 pulled forward) rather than a
   nicer unsupported-message.
R6 ASCII-only case folding pinned: CAFÉ does not find Café. Accepted.
R7 Literal glob metachars: regex escapes are the escape hatch; globs
   never escape (fnmatch fidelity).
New suite: tests/e2e_intent/ in infinite-craft-cli — owner-stated
outcomes as oracle, distinct from parity (host-vs-host) and unit
(code-derived). R2/R5 cases land pending until sudoc v0.2.0 ships the
engine changes; rules_sudo toolchain tag gains explicit-sha256
override so consumers can pin newer sudoc than their rules tarball's
manifest knows.

## 2026-07-22 — v0.2.0 shipped; intent suite catches its first real bug

- sudoc v0.2.0: \b/\B + true mid-pattern anchors via zero-width Assert
  states (replaced the anchor-pass machinery — one Pike VM pass; globs
  reuse Start/End asserts for whole-string match). Rulings R2/R5 live.
- The pin bump exposed a rules_sudo resolution bug on day two:
  last-write-wins tags let the ruleset's self-invocation override the
  consumer's requested version — v0.2.0 asked, v0.1.0 served, checksums
  valid, zero errors. Every mechanical layer (conformance, lockstep,
  parity) was self-consistent on the WRONG version; only the
  owner-intent suite ("\b should work") noticed. Fixed as 0.2.1
  root-module-wins. Lesson recorded: version-resolution precedence is
  semantics, and self-invoking extensions must never outrank the root.
- Also mine: flipped fixture stubs to active without content (caught by
  collection error), unsorted expected-names (caught by the suite).
  intent suite: 30/30 active. icc full: bazel 24/24, pytest 667 passed.

## 2026-07-23 — Zig scratch-arena reclamation (#27 completion): proceed, but harden the safety net first

Borrow fix (dc813a6) landed: kernel 26.6 GB -> 1.8 GB, regex 3.7 GB -> 21 MB,
CI green. Residual 1.8 GB is per-iteration BFS transients held in the
never-freed global arena. Owner greenlit reset-scratch reclamation and
correctly preferred reset(.retain_capacity) over per-iteration child arenas.

Advisor pass (Fable, context-clean) changed the design in three ways I am
adopting:
1. Route by DESTINATION, not by whether store() copies. My original rule (c)
   "escape-copies via store()" is UNSOUND: store() elides the copy for
   non-aliasing RHSs (fresh list, call result, builtin), so `outer = f(x)`
   inside a loop binds a scratch value to a persistent local with no copy ->
   use-after-reset. Sound rule: when an assign target root / mutating-builtin
   receiver / inout-or-param root is declared OUTSIDE the scratch scope,
   evaluate the ENTIRE RHS with the persistent allocator active. Coarse
   (a callee's transients go persistent for that statement) but sound.
2. Plumbing: one global active-allocator pointer, swapped with Zig `defer`
   restore (fires on trap unwinding too). Threading a param is rejected —
   every container API (SudoList.append, SudoMap.put, box, key_dup) already
   funnels through rt.allocator().
3. One scratch arena, claimed dynamically by the OUTERMOST active loop
   (runtime already-active flag; only the claimant resets). Naive
   nested-loop sharing is unsound (inner reset frees the outer iteration's
   live transients).

DECISION: proceed (in scope, owner-greenlit, reversible behind full
verification), BUT the "lockstep self-checks a mis-scoped reset" property I
relied on is UNRELIABLE — retain_capacity rewinds without poisoning, so a
stale read returns valid old bytes and can pass lockstep for many iterations.
So I am making that property TRUE by construction before trusting it: in
debug/ReleaseSafe builds, memset the rewound scratch region on every reset so
any use-after-reset reads poison and diverges. The acceptance bar is not just
"kernel MB-scale + seven targets green" but ALSO a poisoned-build run of the
kernel + conformance that would actively catch a mis-scoped reset. If that
can't be made green with confidence, I fall back to landing what's proven and
documenting the residual rather than shipping a subtle memory risk.

Not overfit: route-by-destination + dynamic-claim scratch + poison-on-reset is
a GENERAL per-frame-allocation mechanism (idiomatic Zig arena reset), correct
for any hot loop with transients, not tuned to the kernel BFS.

## 2026-07-23 (addendum, same day) — Reversing the "proceed now": defer the reclamation

Revising the entry directly above. That entry said "proceed, harden the safety
net first." After reading the actual emitter I am reversing to DEFER, and
recording why so the trail is honest.

What changed my mind: the emitter has NO scope/local-declaration tracking, and
route-by-destination, worked through nested loops + calls-inherit-ambient +
returns + inout lifetimes, is not a flag — it is a conservative ESCAPE ANALYSIS
(per-function fixpoint over the escaping set) plus a global active-allocator
indirection through the core allocation path plus dynamic loop-claim plus
poison-on-reset. That is a large, correctness-sensitive change to the
allocation path of EVERY generated Zig program, and its payoff is memory
FOOTPRINT on one stress benchmark — Zig's output is already correct, just fat.
The advisor's own caveat is decisive: a mis-scoped reset reads valid stale
bytes and can pass lockstep SILENTLY. Incurring that risk unsupervised, while
Zach is away, for a footprint win, contradicts his stated priorities
(correctness first, wary of overfitting, cost-conscious).

DECISION: bank the 15x borrow win (landed, CI-green). Do NOT one-shot the
reclamation. The full design is captured, execution-ready and advisor-vetted,
in notes/zig-scratch-reclamation-design.md (two-arena + active pointer,
default-persistent scratch opt-in, dynamic outermost-loop claim, conservative
escape-set fixpoint, poison-on-reset as the acceptance oracle, layered so
Layer 1 is a pure-plumbing rollback point). Recommend Zach greenlight the
deliberate layered build (I supervise each layer against the poisoned oracle)
rather than accept a rushed change, OR accept the bounded, documented 1.8 GB
residual on the kernel stress test. This is a genuine risk/effort change from
the "mechanical scoped-arena" the task originally sounded like, which is why it
goes back to him rather than getting decided unilaterally at the keyboard.

## 2026-07-23 (later) — Zig memory OVERHAUL greenlit: honest scoped allocators (supersedes the deferral above)

Zach reframed: the defect is not footprint, it is that the Zig memory model is
DISHONEST and non-idiomatic — a hidden process-global arena reached from inside
container ops, freed only between tests. He greenlit a full overhaul and left
the form to me. So the deferral two entries up is superseded: we ARE doing this,
as a correctness-of-representation fix, not a footprint optimization.

ARCHITECTURE (advisor-vetted, Fable, context-clean):
Principle — "create in an in-scope arena; at every STORE BOUNDARY deep-copy into
the DESTINATION's own allocator." This is exactly sudo value semantics (store()
already deep-copies); today the copies just target a global sink that never
drains. The boundary set is PROVABLY COMPLETE because sudo has no observable
aliasing (spec 2.1) — every place roots at a named local/param, so no escape
channel is missed and NO whole-program fixpoint is needed (Tier 2 included).

- Containers ALLOCATOR-CARRYING (managed): SudoList/Map/Set gain an `alloc`
  field set at creation; mutating ops use self.alloc; global allocator() removed.
- inout: managed containers handle mutation-through-inout for free, BUT
  whole-value replacement of an allocator-less composite (`p := newrec`) has no
  container to read .alloc from and ret_alloc is wrong for a forwarded inout ->
  thread ONE `alloc` param per needs_dup inout param, used only at replacement.
- Functions that allocate gain `ret_alloc: Allocator`; open
  `var scratch = ArenaAllocator.init(ret_alloc); defer scratch.deinit();`;
  internals default to scratch; callees get the ambient scratch as their
  ret_alloc. Skip the arena in non-allocating functions.
- store() targets the destination allocator and CLOSES its copy-elision for
  non-aliasing RHS at an escaping destination (else UAF once source is scratch).
- Test harness: per-test `ArenaAllocator` via defer replaces the global reset;
  traps free via defer (cleaner than C's intrusive free-list). const-init needs
  an explicit allocator param + a per-test owner.

TIERS (Zach's insight unified them): the LOOP-ITERATION boundary is just another
store boundary — value surviving to the next iteration == value written to
loop-carried storage == already a copy-into-longer-lived-allocator. So Tier 2 =
arena per loop level (stack keyed by loop DEPTH), reset per iteration; loop body
ambient = innermost loop arena; calls in the loop get that arena as ret_alloc;
cross-iteration/-depth assigns copy into the declaring scope's arena; FOR-IN
SNAPSHOTS live in the ENCLOSING arena (they span the loop). Needs only light
lexical depth tracking, not dataflow. Relies on block scoping (loop-inner locals
unreadable after the loop) — already enforced by Zig's block emission.

Ship Tier 1 first (honest + idiomatic on its own); Tier 2 is the hot-loop
follow-up. ACCEPTANCE ORACLE: run the full seven-target suite in Debug under
std.heap.DebugAllocator (real use-after-free trapping) — the global arena has
been MASKING lifetime bugs, so post-overhaul a single shallow copy / missed
elision is silent ReleaseSafe corruption; DebugAllocator makes it loud.

Design doc notes/zig-scratch-reclamation-design.md is now partly superseded (it
described the footprint-only two-arena scratch); this entry is the current
architecture. Will refresh that doc when the plan is decomposed.

## 2026-07-24 — Honest-arena overhaul: Stages 1/2a/2b landed and measured

Executed the overhaul via the grok-yolo lane (grok --permission-mode
bypassPermissions, driven directly; the fable-advisor wrapper's acceptEdits
was the block — see memory grok-lane-yolo-invocation). Both external CLI lanes
were down first (grok sandbox-blocked, codex not installed); grok-yolo restored
the cheap lane.

- Stage 1 (6934af6): containers carry an alloc field; box/copy_*/mutating ops
  take an explicit allocator via a single codegen seam cur_alloc(). Behavior-
  identical.
- Stage 2a (4eea3bf): uniform ret_alloc first param on every function + Ty::Func
  pointer type + all call sites. Behavior-identical. (Uniform because
  FuncRef/CallValue/address-taken share one signature — all-or-none.)
- Stage 2b (ad58c61): per-function scratch arena backed by a REAL freeing
  allocator (rt.backing = page_allocator), defer-deinit; cur_alloc()->scratch;
  store() copies into the destination's allocator at each escape boundary
  (return->ret_alloc, container->carried .alloc, inout detected via place root).
  Caught + fixed my own spec bug mid-flight: scratch was first backed by
  ret_alloc (arena-on-arena never reclaims); switching to a real backing both
  delivers reclamation AND turns any missed escape-copy into a hard UAF crash
  the suite catches (instead of a silent leak).

RESULT (kernel, ../infinite-craft-cli craft.sudo, 18 tests, zig ReleaseSafe):
peak RSS 1.8 GB -> 166 MB (~11x this stage; ~160x from the original 26.6 GB).
All green with zero crashes/segfaults under the real allocator: cargo test
--workspace, conformance 11/11 (7 targets), examples+stdlib. 18/18 kernel tests.

Remaining: Stage 3 (per-test arena + delete the global arena/reset + DebugAllocator
oracle build) and Stage 4 (per-loop arenas + the iteration boundary — drives the
compute_layers inline per-iteration transients, the residual 166MB, toward the
C ~3MB / Python 25.6MB working set). Stage 2c residual: per-inout allocator for
forwarded non-container inout whole-value replacement (TODO in code).

## 2026-07-24 — Honest-arena overhaul COMPLETE (Stages 3 & 4)

- Stage 3 (c0c7a9c): per-test ArenaAllocator (_sudo_ta) freed by defer; global
  sudo_arena + between-tests reset DELETED; consts build into the per-test arena
  via sudoInitConsts(ca). rt.backing() is build-mode-aware -> DebugAllocator in
  Debug with a backingReport() leak check: the acceptance oracle. Hygiene fix:
  generated arena identifiers use the reserved _sudo_ prefix (_sudo_scratch,
  _sudo_ta) - caught when the DebugAllocator oracle hit the kernel's user var
  named 'ta'.
- Stage 4 (a3dbccc): per-outermost-loop arena, reset each iteration; loop-carried
  values escape to the enclosing allocator. Bug caught by the kernel oracle:
  SudoMap/SudoSet retain the key/element VALUE (not just its encoding), so map
  keys and set elements must copy into the container allocator like list
  elements (masked pre-Stage-4 because cur_alloc==scratch==container.alloc).

RESULT: kernel peak RSS 26.6 GB -> 4.2 MB (~6300x); on par with C (~3 MB), below
Python (~25 MB). All seven targets lockstep green, conformance 11/11, kernel
18/18 leak/UAF-free under a Debug DebugAllocator build. Memory model is now
honest + idiomatic: explicit scoped allocators, defer-freed per call and per
loop iteration, value-semantics copies at real store boundaries, zero global
state.

FOLLOW-UPS (backlog):
- CONFORMANCE COVERAGE GAP: both Stage-3/4 bugs (the 'ta' identifier collision
  and the map/set key-retention) were caught ONLY by the infinite-craft kernel,
  not the sudocode conformance suite. Add a conformance module with (a)
  adversarial identifiers named after generated infra (scratch, ta, loop, snap,
  ret_alloc, ...) to catch hygiene regressions in ANY backend, and (b) composite
  map-keys / set-elements inserted AND retrieved inside a hot loop, to catch
  container value-retention lifetime bugs. This is the general hardening the
  earlier conformance-depth discussion pointed at.
  [CLOSED — see conformance/semantics/generated_idents.sudo and
  container_lifetime_loops.sudo]
- STAGE 2C RESIDUAL (TODO in code): per-inout allocator param for forwarded
  NON-container inout whole-value replacement (currently falls back to ret_alloc,
  correct only when the inout arg is the caller's own local). No corpus case hits
  it yet; the adversarial conformance module above should include one.

## 2026-07-25 — Final honesty/idiom review round (post-overhaul)

Ran a single context-clean fable-advisor review of the FINAL generated Zig +
codegen (goal: confirm honest + idiomatic, not just green). Verdict: model is
genuinely honest, but four items — fixed all:

1. (real bug) Non-container composite inout (record/option/enum) whole-value-
   replaced INSIDE a loop: the replacement was built in the caller's per-
   iteration loop arena and freed by the next reset while still live. Not even
   forwarding-specific — a direct call in a loop breaks it. Fix (a0c2d18):
   caller re-homes the arg into its true-lifetime allocator after the call
   (function scratch for a loop-carried local, _sudo_ret_alloc for a forwarded
   inout); composes through chains. New repro conformance module
   inout_loop_lifetime.sudo.
2. (honesty) The DebugAllocator oracle was OVERSTATED: reset(.retain_capacity)
   never returns memory to backing, so use-after-reset READS are invisible to
   it — which is why the Stage-4 map/set bug surfaced as a lockstep AssertFailed,
   not an oracle crash. Corrected (a0c2d18): loop reset uses .free_all in Debug;
   runtime documents "strong evidence, not lifetime proof." A stale read before
   the next realloc is fundamentally un-catchable by an allocator — honest
   limitation, documented.
3. (honesty) 16KB structural-key buffer silently truncated -> key collisions.
   Now @panic (a0c2d18).
4. (idiom) Loop arenas were always emitted (265 in the kernel, many pure-scalar).
   Now emitted only when the body can allocate; plain `while (cond)` restored;
   `_ = {};` noise dropped (9280f84). Kernel RSS held at 4.14 MB; loop arenas
   265 -> 228.

OPERATIONAL LESSON (cost me hours): a grok-yolo background run STALLED IDLE for
~8 hours (grok CLI hang, not a code problem — no runaway process; its edits were
complete and correct). I trusted "it's progressing" instead of checking etime.
Going forward: bound grok runs and/or check `ps -o etime` on background tasks;
do not assume forward progress. See [[grok-lane-yolo-invocation]].

STATE: overhaul + review fixes + polish all committed (dc813a6..9280f84), all
green (cargo test --workspace, conformance 14/14 x7 targets, examples, kernel
18/18 Debug+ReleaseSafe at 4.14 MB). NOT yet pushed — awaiting owner.

## 2026-07-27 — Task #17 (record-field boundary intent): full feature greenlit

Owner chose the full feature (not the checker-only slice). Scope: per-field
boundary intent on record/enum declarations so text fields survive host
boundaries, + the two checker-correctness fixes, + adapters in all 7 backends.

Design (from the #17 exploration; architecture is well-determined):
- Ty stays erased (text -> List(Int); the language semantics ARE List<int>).
- IR records/enums gain per-field BoundaryTy (additive). Populated by the types
  crate via type_expr_to_boundary_ty BEFORE erasure.
- validate_export_boundary descends into record/enum fields (currently treats
  them as leaves) to reject func-typed members, WITH a visited-set guard —
  because legal recursive enums (enum Tree{Leaf,Node(Tree,Tree)}) in an export
  signature would otherwise loop forever once the walker descends. Guard mirrors
  the existing is_hashable walk (crates/types lib.rs:894).
- Cross-module named-type resolution in export signatures (flagged in
  backend_js/src/boundary.rs).
- All 7 backend adapters consume per-field BoundaryTy to map text record/enum
  fields to host String; remove the backend_js local workaround.
- Wire protocol bump (spec/protocol/ir-schema.json) + spec/lockstep.md §5.4
  update (drop the "keep records out of export signatures" limitation).

Staging (each independently green): A = IR+wire+types-populate (backends ignore
the new field; golden-IR verify). B = checker fixes (validate descends + guard +
cross-module). C = all-backend adapters + boundary round-trip tests + remove the
JS workaround. Advisor-vetting the design + IR representation + staging before
dispatch, given the wire-protocol change is awkward to revise.

## 2026-07-27 — #17 design VETTED (advisor); refinements

- IR representation: struct field, REQUIRED not Option. fields: Vec<(String,Ty)>
  -> Vec<IrField{name,ty,boundary}>. Tuple->struct is deliberate: compiler
  enumerates every field-iteration site in all 7 backends. Wire schema: 2-tuple
  arrays -> named-key objects.
- Generics OK: function-only, exported funcs can't be generic, monomorph
  substitutes at TypeExpr level pre-resolution -> deriving field boundary from
  the record decl's surface TypeExpr is sound.
- Stage B must ALSO verify the checker rejects cross-module bare-name collisions
  (BoundaryTy::Named drops the module qualifier; resolution rests on bare-name
  uniqueness across the import closure - same assumption Ty::Record already makes).
- Stage B is a DELIBERATE BREAKING CHANGE: exporting a record with a func-typed
  field currently compiles (adapter silently skips); descending validate turns
  it into a hard error (correct, fail-loud). Update the conformance corpus.
- Stage C is NOT uniform: scope per backend by existing adapter capability
  (C is scalar-only -> possibly zero record work; JS has the seam bty_of_ty ->
  bty_of_boundary). Not blanket all 7.

## 2026-07-27 — #17 COMPLETE (record-field boundary intent, all 3 stages)

- Stage A (84175f3): IrField{name,ty,boundary}; types crate populates boundary
  from surface TypeExpr pre-erasure; wire schema 2-tuple->named-object + protocol
  bump; all 7 backends' field sites updated; golden re-blessed. Behavior-neutral.
- Stage B (ea0a01e): validate_export_boundary descends into record/enum fields
  (was leaf) via boundary_contains_func with a visited-set guard (recursive enums
  terminate); func fields in exports now hard-error (deliberate breaking change,
  no corpus hit). Did by hand to skip grok's exit-stall.
- Stage C (b38a697): JS adapter reads IrField.boundary (not erased ty) for
  record/enum fields; KNOWN GAP closed; e2e test proves a record text field
  round-trips as a JS string under node. spec/lockstep.md limitation removed.

SCOPE FINDING (per advisor's per-capability guidance): only the JS adapter has
record/enum conversion. Python passes records through (conv_in/out Named => None),
C is scalar-only, swift/rs/zig/hs have NO host adapter. So Stage C = JS only; the
per-field intent now rides in the IR/wire for the others to consume as their
adapters grow (same pattern as C being scalar-only).

DEFERRED (small, logged as follow-up): the CHECKER's cross-module func-field
rejection. validate descends into LOCAL records/enums; a named type from an
imported module isn't in the local ctx, so it's treated as a leaf (NO regression
vs today's no-descent; untested scenario). The FEATURE's cross-module need (JS
adapter mapping an imported record's text fields) works via BoundaryTy::Named
resolution. Closing the checker sliver needs threading imported type tables into
validate_export_boundary.
[VERIFIED UNREACHABLE 2026-08-07 — the scenario cannot be constructed in v1: a
module-qualified type in a signature is rejected at resolve ("unknown type",
see the types-crate diagnostic), and calling a cross-module func whose
signature mentions a module-local record is rejected by the spec §9 boundary
rule. Closed by construction; revisit only if qualified type references ever
land.]

GROK RELIABILITY: stalled again on Stage A (exit-hang after completing the work,
caught at ~14min idle by a stall-detector). Did B and C by hand - faster than the
dispatch+babysit+verify cycle for focused changes.

BAZEL VERSION PIN (Phase 1b prep): pinned Bazel to 8.3.1 via .bazelversion.
Phase 0 used bazelisk's default (9.2.0), which only ever exercised rules_rust —
9.2-compatible. Phase 1b needs the backend-toolchain rulesets, and Bazel 9.2 is
ahead of that ecosystem: hermetic_cc_toolchain (3.1.1/3.2/4.0), rules_nodejs
(6.3.0), and toolchains_llvm (1.2.0) all fail on 9.2 with Bazel-9 API breakages
(e.g. "at index 0 of provides, got element of type NoneType"). Verified fix:
hermetic_cc builds cleanly on 8.3.1 (and 7.4.1), and the full Phase 0+1a Bazel
suite (35 tests) passes unchanged on 8.3.1. Chose 8.x over 7.x LTS: newer, closer
to the original 9.2 intent, and both rules_rust + hermetic_cc work. rules_zig
0.16.0 loads on 8.3.1 but a transitive test-toolchain (bats via aspect_bazel_lib)
downloads from github.com, which THIS container's egress proxy 403s — an env
constraint, not a Bazel-8 issue; CI (open internet) is unaffected. The spec's
"Bazel 9.2" was aspirational; 8.3.1 is the working pin for the backend phases.

## 2026-08-07 — Type names go underscore-free (grilled + approved)

Record and enum names may not contain `_` anywhere (leading, interior, or
trailing). Every other identifier class — functions, variables, params, consts,
variant names, field names, module names — stays unrestricted.

Why: record/enum names are the only bare user data embedded inside
`ty_mangled`'s generated symbols; `_` is the component separator, so an
underscore in a type name breaks unique decodability. Two confirmed collision
shapes: `record List_3i64` vs `List<int>` (both produce `List_3i64`); and given
records `a_b`/`c`/`a`/`b_c`, `Map<a_b,c>` vs `Map<a,b_c>` (both produce
`Map_a_b_c`).

Alternatives considered and rejected:
- A structural-tag prefix blocklist (banning names starting with `List_`/`Map_`/
  etc.) under-protects: it does not stop the `Map<a_b,c>` vs `Map<a,b_c>` shape,
  which has nothing to do with a blocked prefix.
- Regrammaring with a `Sudo_`-style prefix/escape on every generated symbol:
  causes symbol churn across every backend and a readability tax on every
  generated identifier.
- A `user_` prefix partition scheme: taxes every line of generated output, not
  just the rare colliding case.

The oracle (`sudoc/crates/types/src/mangle_check.rs`'s `CollisionMap`) is
retained permanently as a defensive check. It is now source-unreachable from
valid programs but covered by direct unit tests that construct colliding `Ty`
values, so the logic stays exercised against future grammar changes.

Rollout: breaking change with zero corpus impact — no underscore-bearing
record/enum name exists in stdlib, examples, the conformance suite, or the
infinite-craft kernel. Ships as the headline breaking-change note of the next
toolchain release (v0.4.0). No deprecation cycle — the checker's
rename-suggestion diagnostic (mechanically derived CamelCase name) is the
migration tool.
