# Bazel Migration — Phase 1a Implementation Plan

> **For agentic workers:** implement task-by-task with a verify checkpoint after
> each. Steps use checkbox (`- [ ]`) syntax. Commit per task as `bazel(phase1a): …`.

**Goal:** Stand up the *contracts* the decomposed lockstep DAG needs, add the
`lockstep_diff` binary, and get `bazel test //conformance:all` green for the two
lowest-friction backends (**py, js**) — while cargo and the monolithic
orchestration keep working (coexistence until Phase 4).

**Spec rows:** design §3 (contract details), §6 (rules_sudo API), §8 Phase-1a row.
**Builds on:** Phase 0 (root `MODULE.bazel`, 14 crate `BUILD.bazel`, `.bazelrc`,
BuildBuddy cache, Bazel CI job).

## Global constraints (unchanged from Phase 0)

- **Coexistence.** Do not delete any cargo file or the monolithic `sudoc
  test`/`conformance`. `cargo build` + `cargo test --workspace` stay green after
  every task. Phase 1a *adds* `lockstep_diff`; the `harness`→`lockstep_diff`
  *narrowing/deletion* is Phase 4.
- **Clippy the CI way before every push:** `cd sudoc && cargo clippy --all-targets
  -- -D warnings` (a `map_or`→`is_some_and` slip once broke CI for days).
- **Don't break shipped rules_sudo.** v0.2.1 `sudo_lockstep_test` (PATH-inheriting)
  stays intact — infinite-craft-cli depends on it. Phase 1a *adds* the new
  hermetic decomposition alongside it; the breaking rename → rules_sudo 1.0.0 is
  Phase 5. New hermetic rules live in `rules_sudo/private/lockstep.bzl`.
- **No secret in-repo.** BuildBuddy key stays in gitignored `user.bazelrc` /
  CI secret. New CI steps reuse the existing gating.

## Contract shapes (the through-line)

Per `.sudo` module the decomposed DAG is `lockstep_diff(tests-manifest, N
captured-run files)`:

- **tests-manifest** — JSON array of `sudoc_ir::names::test_fn_names(entry.tests)`,
  emitted by a per-module frontend action (`sudoc emit-tests`). Without it a test
  absent from *all* backends vanishes silently (§3).
- **captured-run file** — `{stdout, stderr, exit_code}` JSON written by a
  **never-fail wrapper** (`capture_run`, always exits 0) around each backend's
  run leaf. A crashing runner (StackOverflow, ASan abort) must still produce a
  file so the "no result" divergence signal survives (§3).
- **`lockstep_diff`** — hermetic `rust_binary` that reads the manifest + the N
  captured files, runs `parse_tap` + the stderr sanitizer scan + verdict
  computation, and reproduces `render()`'s divergence table. Exits nonzero iff not
  all-pass. Shares code with the `harness` crate (does not delete it).
- **`emit-ir`** — `sudoc emit-ir` writes IR-as-JSON, the documented sudoc↔emitter
  boundary artifact (`spec/protocol.md`). Not consumed by py/js (they use in-tree
  codegen) but built now because it's the Phase-1a contract the Haskell emitter
  (Phase 3) needs, and it pins the boundary early.

---

### Task 1 — #38: data-reading tests regain Bazel coverage

The 5 tests that read repo files via `env!("CARGO_MANIFEST_DIR")` are excluded
from Bazel today (2 tagged `manual`: `syntax_parser`, `types_checker`; 3 not
wired at all: `types_golden`, `ir_schema_golden`, `harness_wire_trip`). The
checker test carries the F8/F10/F12 export-boundary coverage — restoring it is
the priority. Fix: switch each from the compile-time `env!` (sandbox source path,
no data siblings) to the **runtime** `std::env::var("CARGO_MANIFEST_DIR")` and
declare the read files as `data`. Dual-mode: `cargo test` sets
`CARGO_MANIFEST_DIR` at runtime to the crate dir (real tree); rules_rust's process
wrapper sets it into the runfiles tree where `data` is staged repo-relative, so
the existing `../../../examples` traversal resolves under both.

**Files:**
- Modify (test srcs): `sudoc/crates/syntax/tests/parser.rs`,
  `types/tests/checker.rs`, `types/tests/golden.rs`, `ir/tests/schema_golden.rs`,
  `harness/tests/wire_trip.rs` — replace `env!("CARGO_MANIFEST_DIR")` /
  `concat!(env!("CARGO_MANIFEST_DIR"), …)` with a small `manifest_dir()` helper
  reading `std::env::var("CARGO_MANIFEST_DIR")`.
- Modify (BUILD): `syntax/BUILD.bazel`, `types/BUILD.bazel`, `ir/BUILD.bazel`,
  `harness/BUILD.bazel` — drop `manual`, add `data`, add the 3 missing targets.
- Create: `conformance/BUILD.bazel` (filegroups `golden_srcs`, `semantics_srcs`),
  `spec/BUILD.bazel` (filegroup exporting `protocol/ir-schema.json`).
  (`examples/BUILD.bazel:sudo_srcs` already exists.)

**Interfaces:** produces runnable `//sudoc/crates/{syntax,types,ir,harness}:*`
data-reading `rust_test`s; no `manual` tag.

- [ ] **Step 1:** Add a `manifest_dir() -> PathBuf` helper in each test file (or a
  shared `concat!`→runtime swap). Keep the `../../../…` joins unchanged.
- [ ] **Step 2:** `examples` data → `syntax_parser`, `types_checker`,
  `types_golden` (also `//conformance:golden_srcs`); `ir_schema_golden` →
  `//spec:ir_schema`; `harness_wire_trip` → `//conformance:semantics_srcs`.
  Remove `tags=["manual"]` from `syntax_parser`, `types_checker`. Add the missing
  `types_golden`, `ir_schema_golden`, `harness_wire_trip` targets (harness gets
  its first `rust_test`s).
- [ ] **Step 3 (checkpoint):** `bazel test //sudoc/crates/syntax:syntax_parser
  //sudoc/crates/types:types_checker //sudoc/crates/types:types_golden
  //sudoc/crates/ir:ir_schema_golden //sudoc/crates/harness:harness_wire_trip`
  → all PASS. If `CARGO_MANIFEST_DIR` is unset at runtime under rules_rust,
  fall back to the `@rules_rust//tools/runfiles` crate (`Runfiles::rlocation`);
  note which path was used.
- [ ] **Step 4:** `cd sudoc && cargo test -p sudoc-syntax -p sudoc-types -p
  sudoc-ir -p sudoc-harness` still green (dual-mode holds).
- [ ] **Step 5:** clippy clean; commit
  `bazel(phase1a): #38 — CARGO_MANIFEST_DIR tests run under Bazel (data+runfiles)`.

---

### Task 2 — `emit-ir` + `emit-tests` CLI contracts

**Files:**
- Modify: `sudoc/crates/cli/src/main.rs` (two subcommands + dispatch).
- Create: `sudoc/crates/cli/tests/emit_contracts.rs` (golden-ish assertions).
- Modify: `sudoc/crates/cli/BUILD.bazel` (add the `rust_test`).

**Interfaces:**
- `sudoc emit-ir <src> [-I dir] -o <ir.json>` → `Vec<IrModule>` as pretty JSON
  (reuses `check_program_with`; serialize with serde_json — already an `ir` dep).
- `sudoc emit-tests <src> [-I dir] -o <tests.json>` → JSON array of
  `test_fn_names(entry.tests)` for the entry module.
- Both write to stdout when `-o` is omitted.

- [ ] **Step 1:** Factor the arg-parse (`-I`, `-o`, positional file) shared with
  `build`; add `Some("emit-ir")` / `Some("emit-tests")` arms.
- [ ] **Step 2:** `emit-ir`: check → `serde_json::to_string_pretty(&modules)`.
  `emit-tests`: check → `test_fn_names(&entry.tests)` → JSON array.
- [ ] **Step 3 (checkpoint):** `bazel run //sudoc/crates/cli:sudoc -- emit-ir
  conformance/semantics/arithmetic.sudo` prints valid IR-JSON; `… emit-tests …`
  prints the module's `test_*` names. Round-trip `emit-ir` through
  `serde_json::from_str::<Vec<IrModule>>` in the test.
- [ ] **Step 4:** cargo test + clippy clean; commit
  `bazel(phase1a): sudoc emit-ir + emit-tests (IR-JSON boundary + tests manifest)`.

---

### Task 3 — `capture_run` (never-fail wrapper) + `lockstep_diff` binary

Refactor the harness so the per-name outcome/verdict assembly is a **pure
function of (tests-manifest, N captured runs)**, then expose it through a new
`lockstep_diff` binary. The monolithic `lockstep_with` keeps working by calling
the same pure function after it execs the toolchains.

**Files:**
- Modify: `sudoc/crates/harness/src/lib.rs` — add `pub struct CapturedRun {
  stdout, stderr, exit_code }`; make `sanitizer_report_detail` reusable; extract
  `pub fn diff(module, tests_manifest, runs: &[(String, CapturedRun)]) ->
  ModuleReport` from the `for name in &expected { … }` body of `lockstep_with`;
  refactor `run_target_in` to build a `CapturedRun` then call `diff` (behavior
  unchanged).
- Create: `sudoc/crates/harness/src/bin/lockstep_diff.rs`,
  `sudoc/crates/harness/src/bin/capture_run.rs`.
- Modify: `sudoc/crates/harness/BUILD.bazel` — exclude `src/bin/**` from the lib
  glob; add two `rust_binary` targets + a `rust_test` for `diff`.

**Interfaces:**
- `capture_run --out <file.json> -- <cmd> [args…]` → runs cmd, writes
  `{stdout, stderr, exit_code}`, **always exits 0**.
- `lockstep_diff --module <name> --tests <tests.json> --run <backend>=<file.json>
  …` → `diff` → prints `render()` table; exit nonzero iff not all-pass.
- Placing both under `src/bin/` keeps them in the `sudoc-harness` cargo package
  (coexistence) while the lib glob `exclude=["src/bin/**"]` keeps the library
  clean under Bazel.

- [ ] **Step 1:** Introduce `CapturedRun` + `diff`; re-point `lockstep_with` and
  `run_target_in` at them. `cargo test -p sudoc-harness` green (no behavior
  change — `wire_trip` + any existing harness unit tests still pass).
- [ ] **Step 2:** Write `capture_run.rs` (spawn, capture `output()`, serialize,
  `ExitCode::SUCCESS`).
- [ ] **Step 3:** Write `lockstep_diff.rs` (arg parse, read files, call `diff`,
  `render`, map `all_green` → exit code).
- [ ] **Step 4:** `rust_test` for `diff`: feed synthetic captured runs incl. a
  backend whose run "crashed" (empty stdout + sanitizer stderr + nonzero exit) →
  assert the missing test surfaces as `Outcome::Missing` and a `Divergence` with
  the sanitizer annotation (spec success criterion: crashed runner ⇒ divergence,
  not silent pass).
- [ ] **Step 5 (checkpoint):** `bazel build //sudoc/crates/harness:lockstep_diff
  //sudoc/crates/harness:capture_run`; `bazel test //sudoc/crates/harness:…`.
  cargo workspace still green; clippy clean.
- [ ] **Step 6:** commit `bazel(phase1a): capture_run wrapper + lockstep_diff
  binary (harness diff extracted, not narrowed)`.

---

### Task 4 — py/js lockstep; `//conformance:all`

> **Execution deviation (forced blocker).** `rules_nodejs` 6.3.0 is incompatible
> with Bazel 9.2 (`incompatible_use_toolchain_transition` was removed), and a
> globally-registered node toolchain poisons every build. Hermetic `node` on
> Bazel 9 needs a heavier/newer ruleset. So Phase 1a lands a pragmatic shape that
> still meets the gate via the decomposed contracts:
> - **codegen + tests-manifest = hermetic, cached build actions** (in-tree
>   `sudoc`) — the per-backend cacheable part, spec-aligned.
> - **run leaves + diff = test-time**: `capture_run` (never-fail) + `lockstep_diff`
>   under host `python3`/`node` (`env_inherit=["PATH"]`, `tags=["local"]`), the
>   same interpreter contract as the shipped e2e.
> - rules live in root **`//tools/lockstep.bzl`** (no `rules_sudo` MODULE edits /
>   no released-toolchain fetch); no `rules_python`/`rules_js` dep.
> - **Deferred to 1b:** hermetic run leaves (interpreter-in-action,
>   remote-cacheable captures) + a Bazel-9 hermetic `node`. **Deferred to 5:**
>   packaging as `rules_sudo` macros (the breaking 1.0.0 release).

The original hermetic-toolchain plan below is the 1b/5 target shape.

#### Original plan (1b/5 target): rules_python + rules_js; hermetic py/js lockstep

**Files:**
- Modify: `MODULE.bazel` — `bazel_dep` rules_python + rules_js (+ aspect
  rules_js/nodejs), interpreter + node toolchains; `local_path_override` +
  `bazel_dep` on `rules_sudo` (dogfood in-tree).
- Create: `rules_sudo/private/lockstep.bzl` — the new hermetic rules:
  (a) codegen rule `sudoc build --target <lang> --tests -o <dir> <entry>` (reuse
  the `_sudo_build_library` staging), (b) `py`/`js` run leaves wrapped by
  `capture_run` → captured-run file, (c) a tests-manifest action (`emit-tests`),
  (d) a `lockstep_diff` test target consuming manifest + captured files. Expose a
  new public `sudo_lockstep_test` variant with a **settable `sudoc` /
  `lockstep_diff` / `capture_run` label attr** (mandatory in-tree — no
  `@sudo_toolchain` default until Phase 5 packaging).
- Modify: `rules_sudo/MODULE.bazel` — add rules_js dep (rules_python already
  present).
- Create: `conformance/BUILD.bazel` (extend Task-1's file) — `sudo_library` +
  the new hermetic lockstep test per `semantics/*.sudo`, `test_suite(name="all")`.
- Modify: `.github/workflows/ci.yml` — Bazel job also runs `//conformance/...`.

**Interfaces:** `bazel test //conformance:all` runs py+js lockstep hermetically;
each per-(module×backend) leaf caches independently.

- [ ] **Step 1:** MODULE.bazel toolchains. Checkpoint: `bazel build
  @rules_python//… ` resolves; a trivial `py_binary`/`js` smoke builds. Watch `df`
  (node + interpreter fetches are large; local, not remote-cached).
- [ ] **Step 2:** `lockstep.bzl` codegen + py run leaf + manifest + `lockstep_diff`
  wiring for **one hardcoded module** (`arithmetic.sudo`), py only. Checkpoint:
  its `lockstep_diff` test passes (py alone → all Pass, no divergence).
- [ ] **Step 3:** add the js run leaf; same module now diffs py×js. Checkpoint:
  green, and a deliberately-divergent fixture (or a forced bad capture) makes
  `lockstep_diff` fail with the render table — proving the signal works.
- [ ] **Step 4:** generalize to a `sudo_lockstep_test`-style macro; instantiate
  per `conformance/semantics/*.sudo`; `test_suite(name="all")`.
- [ ] **Step 5 (gate):** `bazel test //conformance:all` green (py, js). cargo +
  monolithic `sudoc test` still green. clippy clean.
- [ ] **Step 6:** CI: add `//conformance/...` to the Bazel job's build+test.
  Commit `bazel(phase1a): rules_python+rules_js; hermetic py/js lockstep;
  //conformance:all green (2 backends)`.

---

## Phase 1a exit gate

- `bazel test //conformance:all` green for **py + js** via
  `lockstep_diff(tests-manifest, captured-run files)`.
- The 5 `CARGO_MANIFEST_DIR` data-reading tests run under Bazel (F8/F10/F12
  checker coverage restored); no `manual` tag.
- `sudoc emit-ir` / `emit-tests` produce the boundary + manifest artifacts.
- A crashed run leaf still surfaces as a divergence (never-fail wrapper +
  manifest), verified by a fixture.
- cargo (`build` + `test --workspace`) and the monolithic `sudoc test` still
  green; clippy `-D warnings` clean; shipped rules_sudo `sudo_lockstep_test`
  untouched.
- CI's Bazel job covers `//sudoc/crates/...` + `//conformance/...`.

## Verification environment note

This execution container has **cargo + clippy but not Bazel by default** (a
`bazelisk` was fetched to `/usr/local/bin/bazel` during planning; Bazel 9.2.0,
first fetch of rules_rust/rustc/crates verified building). Local `bazel
build/test` is the per-task checkpoint; CI (BuildBuddy remote cache) is the
backstop. If a fetch is proxy-blocked locally, fall back to CI for that gate and
say so in the commit/PR.

## Deferred (not Phase 1a)

- c/zig/rs backends (Phase 1b, after the ASan spike), swift (2), haskell +
  delete `hs.sudoc-backend.json` (3).
- Deleting monolithic orchestration / narrowing harness (4).
- rules_sudo 1.0.0 breaking rename, `sudo_external_backend`, reference example,
  infinite-craft-cli migration, matched-pair release (4.5 / 5).
