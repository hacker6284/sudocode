# Bazel Migration — Phase 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the migration — make the Haskell manifest actually disappear (hs via a `sudo_external_backend` Bazel rule), retire the last runtime-discovery code (`--external`, `discovered_backends()`), ship the lockstep macros as `rules_sudo` 1.0.0, and prepare the matched-pair release + downstream migration (dogfooded pre-merge, cut post-merge).

**Architecture:** hs is rewired rule-first-behind-the-old-path → cutover (CI green on the new path) → delete the old path in one commit (so CI proves the cutover before the deletion exists). The `sudo_external_backend` rule synthesizes the run **recipe JSON from Bazel attrs** (`ctx.actions.write`) for `capture_run`; the emitter is an **`sh_binary` running `runghc Emit.hs`** (NOT `haskell_binary` — do not couple the scariest backend to the riskiest ruleset). Lockstep macros move to `rules_sudo/` with `sudoc`/`lockstep_diff` label attrs defaulting to `@sudo_toolchain//:*`; the root repo overrides to `//sudoc/crates/cli:sudoc` via `local_path_override`, so sudocode is its own pre-release dogfood.

**Tech Stack:** Bazel 8.3.1 (bzlmod), rules_rust, rules_python, Starlark macros, sh_binary, the existing `sudoc emit-ir`/`emit-tests`/`emit-recipe` CLI, `capture_run`/`lockstep_diff` Rust bins.

## Global Constraints

- **This branch = code only.** Land on `bazel-migration`: the hs rewire, the deletions, `rules_sudo` 1.0.0, and the `extensions.bzl` dual-asset + `local_binary` changes. The **matched-pair release** (assets come from a git tag) and the **infinite-craft-cli migration** (a different repo) are INHERENTLY post-merge — dogfood them pre-merge, cut them after. Do NOT fake them into a commit here.
- **hs emitter is `sh_binary` running `runghc Emit.hs`** — rules_haskell/`haskell_binary` swap is explicitly OUT of scope (§9: CI-only-verifiable).
- **Hermetic run-leaf conversion is OUT of scope** — zig/swift/hs compilers-in-action stay host-toolchain (spec-sanctioned; §9 CI-only-verifiable). Phase 5 does NOT make c/js/zig/swift/hs hermetic.
- **Delete `discovered_backends()` FULLY** (spec §2.4) — no "keep it internally." After cutover nothing calls it; keeping it means `harness` never finishes narrowing.
- **Protocol-version handshake happens at RUN time in the test launcher** (cheap, hermetic), NOT in the module extension.
- **rules_sudo → 1.0.0** (compatibility-level bump): the new hermetic `sudo_lockstep_test` REPLACES the PATH-inheriting one (breaking, spec §2.5).
- **CI stays green every task.** Rule-first means both hs paths coexist until the cutover; the deletion commit lands only after CI is green on the new path.
- **Bail trigger:** if the hs cutover (Task 2) isn't green within a bounded effort, STOP — merge Phases 0–4 as an honest milestone (the `release.yml` self-gate guarantees no broken rules_sudo pair ships) and do Phase 5 as a follow-up PR.

---

### Task 0: Pin the external-backend recipe interface (investigation + written decision)

**Files:**
- Read: `sudoc/crates/harness/src/bin/capture_run.rs` (recipe format it consumes), `sudoc/crates/cli/src/main.rs` (the `emit-recipe`/`--external` path), `backends/haskell/hs.sudoc-backend.json` (protocol 2: `emit`/`recipe.build`/`recipe.run`), `tools/lockstep.bzl` (current `manifest`/`emitter` attrs + how the hs external leaf is built today), `stdlib/BUILD.bazel` (hs `_EXTERNAL` wiring).
- Create: `notes/phase5-external-backend-interface.md` (the decision record).

**Interfaces:**
- Produces: the exact JSON shape a `sudo_external_backend` rule must `ctx.actions.write` so `capture_run` runs it identically to today's manifest path — the contract every later task depends on.

- [ ] **Step 1: Read how `capture_run` consumes a recipe today**

Run: `sed -n '1,120p' sudoc/crates/harness/src/bin/capture_run.rs` and note the recipe struct/fields (build cmds, run cmd, `{entry}` substitution, env).

- [ ] **Step 2: Read the current hs external path end-to-end**

Run: `grep -n "external\|emit-recipe\|manifest\|emitter\|runghc\|Emit.hs" tools/lockstep.bzl stdlib/BUILD.bazel conformance/BUILD.bazel sudoc/crates/cli/src/main.rs` — trace: how the `.hs` is emitted, how the recipe (ghc build + run) reaches `capture_run`, and where `hs.sudoc-backend.json` is read.

- [ ] **Step 3: Write the interface decision**

In `notes/phase5-external-backend-interface.md`, record: (a) the recipe JSON schema `capture_run` needs; (b) that `sudo_external_backend(name, emitter, recipe_build, recipe_run, ...)` will `ctx.actions.write` that JSON from attrs (replacing the runtime-discovered manifest); (c) the emitter is an `sh_binary` label producing source from IR-JSON (`sudoc emit-ir` output); (d) the `-with-rtsopts=-K8m` and ghc flags move from the JSON `recipe.build` into the rule's `recipe_build` attr verbatim.

- [ ] **Step 4: Commit the decision record**

```bash
git add notes/phase5-external-backend-interface.md
git commit -m "bazel(phase5): pin the sudo_external_backend recipe-JSON-from-attrs interface"
```

---

### Task 1: `sudo_external_backend` rule; wire hs through it ALONGSIDE the old path

**Files:**
- Create: `backends/haskell/BUILD.bazel` (the `sh_binary` emitter: `runghc Emit.hs`).
- Modify: `tools/lockstep.bzl` (add `sudo_external_backend` + its capture_run wiring, synthesizing recipe JSON from attrs).
- Modify: `stdlib/BUILD.bazel` / `conformance/BUILD.bazel` (add a SECOND hs config `hs_new` via the rule, keeping the existing `--external` `hs` config).

**Interfaces:**
- Consumes: Task 0's recipe JSON schema; `sudoc emit-ir` (`cli/src/main.rs`), `capture_run`.
- Produces: `sudo_external_backend(name, emitter, recipe_build, recipe_run)` macro; a `hs_new` lockstep leaf producing a captured-run file identical in shape to the `hs` one.

- [ ] **Step 1: Emitter as `sh_binary`**

`backends/haskell/BUILD.bazel`:
```python
sh_binary(
    name = "emitter",
    srcs = ["emit.sh"],   # a 2-line wrapper: exec runghc Emit.hs "$@"
    data = ["Emit.hs", "SudoRt.hs", "SudoJson.hs"],
    visibility = ["//visibility:public"],
)
```
Create `backends/haskell/emit.sh` (reads IR-JSON on stdin, writes `.hs` to stdout, matching today's `emit` protocol).

- [ ] **Step 2: Add `sudo_external_backend` to `tools/lockstep.bzl`**

Implement per Task 0's decision: a rule/macro that, given `emitter` + `recipe_build` + `recipe_run`, (a) runs `sudoc emit-ir` on the module → IR-JSON, (b) runs `emitter` → generated source, (c) `ctx.actions.write`s the recipe JSON from the attrs, (d) feeds source + recipe to `capture_run` → captured-run file. Reuse the existing capture_run action wiring; only the recipe *source* changes (attrs, not manifest).

- [ ] **Step 3: Wire a parallel `hs_new` config**

In `conformance/BUILD.bazel` / `stdlib/BUILD.bazel`, register a second external backend `hs_new` via `sudo_external_backend(emitter="//backends/haskell:emitter", recipe_build=[["ghc","-O0","-rtsopts","-with-rtsopts=-K8m","-o","{entry}_test","{entry}_test.hs"]], recipe_run=["./{entry}_test"])`. Add it to the backend list for ONE small module first (e.g. `conformance/semantics/arithmetic`).

- [ ] **Step 4: Verify the new hs path is green on that module**

Run: `bazel test //conformance:arithmetic_test` (or the per-module target) and confirm `hs_new` produces the SAME captured outcome as `hs`. Compare: `bazel build` both leaves and diff the captured-run files — they must be byte-identical (same generated .hs → same ghc build → same run).

- [ ] **Step 5: Commit**

```bash
git add backends/haskell/BUILD.bazel backends/haskell/emit.sh tools/lockstep.bzl conformance/BUILD.bazel stdlib/BUILD.bazel
git commit -m "bazel(phase5): sudo_external_backend rule; hs via emitter+attrs alongside --external"
```

---

### Task 2: Cut hs over to the new rule across all lockstep targets

**Files:**
- Modify: `conformance/BUILD.bazel`, `stdlib/BUILD.bazel`, `examples/BUILD.bazel` (replace the `hs` `--external` config with the `sudo_external_backend` one for ALL modules).

**Interfaces:**
- Consumes: Task 1's `sudo_external_backend` + `hs_new`.

- [ ] **Step 1: Replace hs everywhere with the rule-based config**

Swap the old `_EXTERNAL = {"hs": [...manifest...]}` for the `sudo_external_backend`-produced hs across conformance/stdlib/examples. Rename `hs_new` → `hs`.

- [ ] **Step 2: Full lockstep green on all 7 backends via the new hs**

Run: `bazel test //conformance:all //stdlib/... //examples/...` — all 7 backends including the rule-based hs. Every module green.

- [ ] **Step 3: Commit (this is the CUTOVER — old path still exists but is now unused)**

```bash
git add conformance/BUILD.bazel stdlib/BUILD.bazel examples/BUILD.bazel
git commit -m "bazel(phase5): cut hs over to sudo_external_backend across all lockstep targets"
```

**BAIL CHECK:** if Step 2 isn't green after bounded effort, STOP here and merge Phases 0–4 as a milestone; do the rest as a follow-up PR.

---

### Task 3: Delete the runtime-discovery path (one commit — CI already proved the cutover)

**Files:**
- Delete: `backends/haskell/hs.sudoc-backend.json`.
- Modify: `sudoc/crates/harness/src/lib.rs` (remove `discovered_backends()` + `Backend` external-discovery), `sudoc/crates/cli/src/main.rs` (remove the `--external` flag + `emit-recipe --external` if now unused), `tools/lockstep.bzl` (remove the dead `manifest`/`emitter`-via-json attrs and the `--external` leaf).
- Modify: any test referencing discovery (`harness/tests/discovery.rs` if it tests the deleted mechanism — update or remove with a documented mapping).

**Interfaces:**
- Consumes: Task 2 (hs no longer uses `--external`).

- [ ] **Step 1: Delete the manifest + the Rust discovery code**

```bash
git rm backends/haskell/hs.sudoc-backend.json
```
Remove `discovered_backends()` and its callers from `harness/src/lib.rs`; remove `--external` handling from `cli/src/main.rs`. Update/remove `harness/tests/discovery.rs` (document what replaced its coverage).

- [ ] **Step 2: Remove dead Bazel attrs**

In `tools/lockstep.bzl`, delete the JSON-manifest/`--external` leaf path now that all external backends go through `sudo_external_backend`.

- [ ] **Step 3: Build + full lockstep + cargo-free checks green**

Run: `bazel build //sudoc/crates/... && bazel test //sudoc/crates/... //conformance:all //stdlib/... //examples/...` — green. `grep -rn "discovered_backends\|--external\|sudoc-backend.json" sudoc/ tools/ backends/ | grep -v phase5-external` → only doc/history references remain.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "bazel(phase5): delete --external, discovered_backends(), hs.sudoc-backend.json — manifests gone"
```

---

### Task 4: Move the lockstep macros into `rules_sudo` 1.0.0

**Files:**
- Modify: `rules_sudo/defs.bzl` (replace the PATH-inheriting `sudo_lockstep_test` with the hermetic one; export `sudo_external_backend`), `rules_sudo/private/lockstep.bzl` (the moved implementation from `tools/lockstep.bzl`), `rules_sudo/MODULE.bazel` (version 1.0.0, compatibility_level 2).
- Modify: root `MODULE.bazel` (`local_path_override` for rules_sudo already dogfoods; ensure the macros' `sudoc`/`lockstep_diff` label attrs default to `@sudo_toolchain//:*`), `conformance/BUILD.bazel` etc. (load from `@rules_sudo//:defs.bzl`, pass `sudoc="//sudoc/crates/cli:sudoc"`, `lockstep_diff="//sudoc/crates/harness:lockstep_diff"`).
- Delete: `tools/lockstep.bzl` (now in rules_sudo), or leave a thin re-export.

**Interfaces:**
- Produces: `@rules_sudo//:defs.bzl%sudo_lockstep_test`, `%sudo_external_backend` with `sudoc`/`lockstep_diff` label attrs.

- [ ] **Step 1: Move the implementation**

Move `tools/lockstep.bzl` → `rules_sudo/private/lockstep.bzl`; add `sudoc` and `lockstep_diff` label attrs (default `@sudo_toolchain//:sudoc` / `//:lockstep_diff`). Replace `rules_sudo/defs.bzl`'s old `sudo_lockstep_test` with the new one; export `sudo_external_backend`.

- [ ] **Step 2: Bump rules_sudo to 1.0.0**

`rules_sudo/MODULE.bazel`: `version = "1.0.0"`, `compatibility_level = 2`.

- [ ] **Step 3: Root repo consumes rules_sudo, overrides to freshly-built binaries**

Root `MODULE.bazel` keeps `local_path_override(module_name="rules_sudo", path="rules_sudo")`. Conformance/stdlib/examples `load("@rules_sudo//:defs.bzl", "sudo_lockstep_test", "sudo_external_backend")` and pass `sudoc = "//sudoc/crates/cli:sudoc"`, `lockstep_diff = "//sudoc/crates/harness:lockstep_diff"`.

- [ ] **Step 4: Full green through the public API (dogfood)**

Run: `bazel test //conformance:all //stdlib/... //examples/...` — green, now driven by `@rules_sudo` macros against HEAD binaries.

- [ ] **Step 5: Commit**

```bash
git add rules_sudo/ MODULE.bazel conformance/BUILD.bazel stdlib/BUILD.bazel examples/BUILD.bazel
git rm tools/lockstep.bzl
git commit -m "bazel(phase5): rules_sudo 1.0.0 — hermetic sudo_lockstep_test + sudo_external_backend (dogfooded)"
```

---

### Task 5: Extend the module extension — dual-asset fetch + `lockstep_diff` local override + run-time handshake

**Files:**
- Modify: `rules_sudo/extensions.bzl` (fetch BOTH `sudoc` and `lockstep_diff` release assets; expose both), `rules_sudo/versions.bzl` (asset names + sha256 slots for both), `rules_sudo/private/lockstep.bzl` (the test launcher checks the protocol version at run time).

**Interfaces:**
- Produces: `@sudo_toolchain//:sudoc` AND `@sudo_toolchain//:lockstep_diff`; `sudo.local_binary(sudoc=..., lockstep_diff=...)` override.

- [ ] **Step 1: Dual-asset fetch**

Extend `extensions.bzl` `_hub_impl` + `sudo.toolchain`/`sudo.local_binary` to fetch/symlink both `sudoc` and `lockstep_diff` (matched pair). `versions.bzl` gains `lockstep_diff` triples + sha256 slots.

- [ ] **Step 2: Run-time protocol handshake**

In the test launcher (`private/lockstep.bzl`), have `lockstep_diff` assert the manifest/capture protocol version matches the `sudoc` that produced them (cheap runtime check), failing loudly on a mismatched pair.

- [ ] **Step 3: Verify local override path (the infinite-craft-cli dogfood mechanism)**

Point a scratch `MODULE.bazel` at `sudo.local_binary(sudoc="<HEAD>", lockstep_diff="<HEAD>")` and confirm a `sudo_lockstep_test` resolves both. (This is exactly how infinite-craft-cli validates pre-release in Task 8.)

- [ ] **Step 4: Commit**

```bash
git add rules_sudo/extensions.bzl rules_sudo/versions.bzl rules_sudo/private/lockstep.bzl
git commit -m "bazel(phase5): dual-asset (sudoc+lockstep_diff) fetch + local override + run-time protocol handshake"
```

---

### Task 6: Cleanups + the two audit findings

**Files:**
- Modify: `.github/workflows/ci.yml` (dedup the 3× BuildBuddy cache block into one composite/helper), create `tools/backends.bzl` (`ALL_BACKENDS`, `EXTERNAL`) and load it in conformance/stdlib/examples.
- Modify: `sudoc/crates/backend_c/tests/sanitizer.rs` or add a lockstep-level check (audit finding 1), `sudoc/crates/harness/BUILD.bazel` (audit finding 2).

**Interfaces:** none new.

- [ ] **Step 1: Dedup the CI cache block**

Extract the `FLAGS=(...)` BuildBuddy setup into `.github/actions/bazel-remote/action.yml` (composite) or `tools/ci-bazel.sh`; call it from all three steps.

- [ ] **Step 2: Shared backend list**

`tools/backends.bzl`: `ALL_BACKENDS = ["py","js","c","rs","zig","swift","hs"]`, `EXTERNAL = {...}`. Load in the three lockstep BUILD files; delete the inlined literals.

- [ ] **Step 3: Audit finding 1 — sanitizer-recipe coverage**

Add a check (Linux) that the emitted C run recipe for a corpus module carries `-fsanitize=address,undefined` — so a transient host-`cc` probe failure that drops instrumentation is caught, not silent. (A `rust_test` asserting the recipe JSON contains the sanitizer flags, or a lockstep-level gate.)

- [ ] **Step 4: Audit finding 2 — restore harness_lockstep breadth**

Either restore `harness_lockstep` classification to all 7 backends, or document in `harness/BUILD.bazel` why py+c is the sufficient verdict-exhibiting pair.

- [ ] **Step 5: Verify + commit**

Run: `bazel test //sudoc/crates/... //conformance:all` green.
```bash
git add .github/ tools/backends.bzl conformance/BUILD.bazel stdlib/BUILD.bazel examples/BUILD.bazel sudoc/crates/backend_c/ sudoc/crates/harness/BUILD.bazel
git commit -m "bazel(phase5): dedup CI cache block + shared backends.bzl; cover sanitizer-recipe + harness_lockstep breadth (audit)"
```

---

### Task 7: Reference example backend + docs; re-frame the PR

**Files:**
- Create: `rules_sudo/examples/reference_backend/` (a minimal `sudo_external_backend` demo — a shell-script emitter, per spec §7 "worked example, kept minimal").
- Modify: `spec/protocol.md` (document the IR-JSON boundary as the surviving contract), `docs/superpowers/specs/2026-07-30-bazel-migration-design.md` (mark Phase 5 done; note release + downstream are post-merge), the PR title/body (it still says "Phase 0").

- [ ] **Step 1: Minimal reference backend**

`rules_sudo/examples/reference_backend/`: a `sudo_external_backend` using a tiny `sh_binary` emitter that emits, e.g., a Python transpilation, with a `sudo_lockstep_test` proving it lockstep-agrees. Keep it minimal — NOT gold-plated.

- [ ] **Step 2: Docs**

Update `spec/protocol.md` (IR-JSON boundary) and the design spec's status. Re-title the PR: "Bazel migration — full migration (Phases 0–5): Bazel-only build + hermetic-where-feasible lockstep".

- [ ] **Step 3: Commit**

```bash
git add rules_sudo/examples/ spec/protocol.md docs/superpowers/specs/
git commit -m "bazel(phase5): reference sudo_external_backend example + protocol/spec docs"
```

- [ ] **Step 4: Update the PR body** (via `gh pr edit 1`) to reflect the full migration + the post-merge follow-ups (Task 8).

---

### Task 8: POST-MERGE follow-ups (documented here, executed after merge)

NOT executed on this branch — recorded so they aren't lost.

1. **Cut the matched-pair release:** tag `v0.2.0`; the release workflow builds `sudoc` + `lockstep_diff` per-platform, publishes with sha256s; verify `release.yml`'s grep-gate now PACKAGES `rules_sudo` (it self-gated before because it execed the retired `sudoc test`).
2. **Update `rules_sudo/versions.bzl`** with the release's sha256s (inject in the workflow before packaging, or via the `sha256s` override).
3. **Migrate infinite-craft-cli** (`../infinite-craft-cli`): swap its parity tests from the v0.2.1 PATH-inheriting `sudo_lockstep_test` to the 1.0.0 hermetic API; validate against HEAD binaries via `sudo.local_binary` BEFORE the release, then against the real release after.

---

## Phase 5 exit gate

- `backends/haskell/hs.sudoc-backend.json`, `--external`, and `discovered_backends()` are GONE; hs runs via `sudo_external_backend` (`grep` confirms no survivors).
- `bazel test //conformance:all //stdlib/... //examples/...` green on all 7 backends through the `@rules_sudo` public macros.
- `rules_sudo` is 1.0.0; the extension fetches the `sudoc`+`lockstep_diff` matched pair; a run-time handshake rejects a mismatched pair.
- Reference backend builds + lockstep-passes.
- Audit findings addressed (sanitizer-recipe coverage; harness_lockstep breadth).
- CI green (Bazel + bazel-macos).
- PR re-framed as the full migration; Task 8 follow-ups tracked.

## Self-review notes

- **Spec coverage:** §2.4 (Task 3), §2.5/§4 (Task 4), §2.6 (Tasks 1–3), §2.7/§7 (Task 7), §8 Phase 4.5 (Tasks 5, 8), §8 Phase 5 (Tasks 4–8). ✓
- **Fable sequencing honored:** rule-first→cutover→delete (Tasks 1–3); sh_binary emitter (Task 1); recipe-JSON-from-attrs pinned first (Task 0); dogfood via local override, release post-merge (Tasks 4–5, 8); hermetic run-leaf conversion excluded (Global Constraints); bail trigger (Task 2). ✓
- **Audit findings folded in:** Task 6 steps 3–4. ✓
- **Verify at execution:** the exact recipe JSON schema (Task 0), the current `tools/lockstep.bzl` attr names, and the `extensions.bzl` hub shape — read before writing the concrete Starlark.
