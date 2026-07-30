# Bazel Migration — Design

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Goal (backlog #34):** Migrate the sudocode build to Bazel for fast incremental
recompiles and maximum cache utility, done cleanly — well-structured BUILD
files, a good hierarchy, hermetic where feasible, macros where they help.

---

## 1. Context

sudocode is two things that build together:

1. **The `sudoc` compiler** — a 14-crate pure-Rust workspace under `sudoc/`
   (`syntax`, `types`, `ir`, `sdk`, `stdlib`, `backend_{py,c,js,swift,rs,zig,ext}`,
   `harness`, `cli`). Six backends are compiled *into* the binary as Rust crates;
   Haskell is external.
2. **A lockstep conformance harness** — today a monolithic Rust process
   (`sudoc conformance` / `sudoc test`) that, per `.sudo` module, generates code
   for every backend, shells out to the target toolchains (`python3`, `node`,
   `clang`, `swiftc`, `zig`, `ghc`) to compile+run it, canonicalizes each
   outcome, and diffs them. Divergence is a first-class failure.

The external Haskell backend is registered by a single runtime-discovered
manifest, `backends/haskell/hs.sudoc-backend.json` (protocol 2: `emit` +
`recipe.build` + `recipe.run`). The six in-tree backends have no manifests.

`rules_sudo/` is an already-shipped, BCR-publishable Bazel ruleset (bzlmod,
v0.2.1) that lets *downstream* consumers transpile `.sudo` via a pinned released
`sudoc` binary. It already ships `sudo_library`, `sudo_py_library`,
`sudo_js_library`, and a `sudo_lockstep_test` — but that `sudo_lockstep_test`
stages sources and execs the **monolithic `sudoc test` with `tags=["local"]` +
`env_inherit=["PATH"]`** (the non-hermetic PATH-inheriting pattern this migration
deletes), and infinite-craft-cli's parity tests already depend on it. So this
migration is a **breaking change** to a shipped API, not a greenfield addition
(see §2.5 and §8).

CI runs two jobs: macOS (all 7 targets, incl. C sanitizers asan/ubsan) and Linux
(6 targets via manifest discovery, +lsan). The macOS job's `clippy -D warnings`
step is the CI gate.

## 2. Locked decisions

These were settled during brainstorming and are the ground rules for the spec:

1. **Bazel-only; retire cargo as the builder.** Bazel owns build, `rust_test`,
   clippy, rustdoc, release binaries, and rust-analyzer project generation.
   `Cargo.toml`/`Cargo.lock` are **kept** as the dependency source of truth that
   `crate_universe` and rust-analyzer read — but nothing ever runs `cargo build`.
2. **Hermeticity line:** the `sudoc` binary and the lockstep harness (codegen +
   the canonicalize/diff tool) are **fully hermetic**. Per-backend *builds* need
   not be — the only non-hermetic residue is running Swift-on-macOS (Apple SDK is
   un-vendorable) and, defensively, the Haskell run leaf. A non-hermetic backend
   build does **not** threaten the lockstep *result*: value semantics + canonical
   serialization guarantee a correct program's observable outcome is identical
   regardless of which machine's toolchain built it.
3. **Decompose the harness into a Bazel DAG.** Research verdict: injecting
   hermetic toolchains into a monolithic orchestrator's PATH is a recognized
   anti-pattern (Bazel RBE guidance: invoke tools through toolchains "in a
   predictable, preconfigured manner, not via PATH"). Decomposition is
   simultaneously the hermeticity-correct choice and the maximum-cache-utility
   choice — a monolithic action is coarse-cached (one key over all inputs), while
   per-target actions get independent keys so a one-backend change reruns only
   that backend. `sudoc` stays central, used hermetically, as the codegen tool
   and the diff tool.
4. **Delete the monolithic orchestration.** `sudoc conformance`,
   `sudoc test <files>`, `discovered_backends()`, and `--external` are removed.
   No non-hermetic "quick local poke" path survives — that would be a footgun
   producing "works on my machine" lockstep results Bazel's hermetic run would
   contradict. One source of truth.
5. **Lockstep decomposition ships as reusable `rules_sudo` macros.** sudocode's
   own conformance suite is the first consumer; downstream Bazel users get
   hermetic, cached, parallel lockstep testing of their own `.sudo` as a
   first-class rule. The `lockstep_diff` tool is versioned and shipped alongside
   `sudoc` as a **matched pair** (§8).
   - **This is a breaking change.** The new hermetic `sudo_lockstep_test`
     replaces the existing PATH-inheriting one with incompatible semantics, so
     **rules_sudo goes to 1.0.0** (compatibility-level bump). infinite-craft-cli's
     parity tests migrate to the new API as part of Phase 5.
6. **All backend manifests disappear, Haskell included.** The manifest was a
   runtime plugin-discovery record; the Bazel build graph replaces runtime
   discovery. The emitter `Emit.hs` becomes a `haskell_binary` target; the
   build/run recipe becomes Bazel rule attributes; registration is a BUILD-file
   `sudo_external_backend(...)`, not JSON. The sudoc↔emitter **IR-JSON exchange
   protocol** (`spec/protocol.md`) survives as the documented action boundary.
7. **Plugin authoring becomes Bazel-based, clone-free, and turnkey.** A
   third-party backend author depends on `rules_sudo` + the released toolchain
   and authors entirely in their own repo (no sudocode clone, no upstream PR).
   The zero-Bazel JSON-manifest floor is intentionally retired (it required the
   runtime orchestration we're deleting). Mitigation: ship a documented
   `sudo_external_backend` rule + a worked reference example backend.

## 3. Architecture — decomposed lockstep DAG

Per `.sudo` module, a `sudo_lockstep_test(...)` expansion produces:

```
             sudoc (rust_binary, hermetic)            ← codegen tool
                        │  one codegen action per (module × backend)
        ┌───────┬───────┼───────┬───────┬────────┬─────────┐
       .py     .c      .js     .rs     .zig     .swift     .hs      ← generated srcs
        │       │       │       │       │         │         │
   py_binary cc_binary js_bin rust_bin zig_bin swift_bin  (emit→hs_binary)
        │       │       │       │       │         │         │       ← build + run,
        └───────┴───────┴───────┴───────┴─────────┴─────────┘         own toolchains,
                        │  each run leaf: NEVER-FAIL wrapper, exit 0,      own cache keys
                        │  captures {stdout, stderr, exit-code}
                 lockstep_diff (rust_binary, hermetic)               ← the "harness", narrowed
                        │  f(tests-manifest, N captured-run files) → equivalence
        //conformance:<module>_test   +   test_suite //conformance:all
        tools/lockstep  (script over `bazel test` for interactive local UX)
```

**Two contract details that make the decomposition correct** (the current
monolithic harness is *not* quite a pure function of N stdout streams — verified
by reading `harness/src/lib.rs`; see §6):

- **A crashing runner exits nonzero** (StackOverflow, ASan abort). A naive Bazel
  run action would *fail*, the diff would never run, and we'd lose the "no result
  (sanitizer-flagged crash)" divergence signal that makes the harness valuable.
  So each per-backend run leaf is a **never-fail wrapper**: always exits 0, writes
  a captured-run file `{stdout, stderr, exit-code}`. Canonicalization and the
  stderr sanitizer-signature scan move *into* `lockstep_diff`.
- **The expected-test list comes from the front end** (`test_fn_names` on the
  checked program); `Outcome::Missing` — how a crashed/absent test is caught — is
  computed against it. So a codegen action also emits a per-module **tests
  manifest**, and `lockstep_diff = f(tests-manifest, N captured-run files)`.
  Without it, a test absent from *all* backends vanishes silently.

- **Hermetic & remote-cached:** `sudoc`, every codegen action, `lockstep_diff`,
  and the py/c/js/rs/zig build+run leaves.
- **Non-hermetic residue (tagged `no-remote-cache`):** the Swift-on-macOS run
  leaf and (defensively) the Haskell run leaf. Everything else caches.
- **The Rust `harness` crate narrows** to the `lockstep_diff` tool (canonicalize
  + compare + the `render()` divergence table). Bazel's DAG does the orchestration
  the crate does today. The narrowing is a Phase-4 *deletion*; Phase 1 *adds*
  `lockstep_diff` as a new binary sharing the parse/canonicalize code (§8). This
  is the single largest piece of work.

## 4. Hierarchy & module structure

```
sudocode/
├── MODULE.bazel          # ROOT module: bazel_dep on rules_rust, hermetic_cc_toolchain,
│                         #   rules_zig, rules_python, rules_js, rules_swift, rules_haskell;
│                         #   rules_sudo via local_path_override (dogfood in-tree)
├── .bazelrc              # common + :remote + :ci configs
├── BUILD.bazel           # gen_rust_project alias; top-level test_suites
├── sudoc/                # THE COMPILER (Rust)
│   ├── Cargo.toml/.lock  # KEPT — crate_universe dep source + rust-analyzer metadata
│   └── crates/
│       ├── syntax/BUILD.bazel      # rust_library + rust_test (+ rust_clippy, rust_doc)
│       ├── … (11 more)
│       ├── cli/BUILD.bazel         # rust_binary → //sudoc/crates/cli:sudoc
│       └── harness/BUILD.bazel     # NARROWED → rust_binary lockstep_diff
├── rules_sudo/           # PUBLISHED ruleset — own MODULE.bazel, stays BCR-publishable
│   ├── defs.bzl          # sudo_library (existing) + sudo_lockstep_test (REPLACED, hermetic) /
│   │                     #   sudo_external_backend (NEW)
│   ├── private/lockstep.bzl        # per-backend build+run rules + diff wiring
│   └── examples/reference_backend/ # worked third-party-plugin example (decision 7)
├── conformance/semantics/BUILD.bazel   # sudo_lockstep_test per .sudo → //conformance:all
├── stdlib/BUILD.bazel                  # sudo_library + sudo_lockstep_test per module
├── examples/BUILD.bazel
├── backends/haskell/     # keeps Emit.hs, SudoRt.hs, SudoJson.hs — LOSES the .json
└── spec/ notes/ docs/    # docs, no BUILD
```

**Module structure:** the root repo is one Bazel module; `rules_sudo` stays a
separate, independently-versioned module consumed via `local_path_override`
(dogfooding). The lockstep macros take a `sudoc` label attr defaulting to the
released `@sudo_toolchain//:sudoc`; sudocode's own conformance overrides it to
the freshly-built `//sudoc/crates/cli:sudoc`. Downstream gets the release; we
test HEAD. No chicken-and-egg.

## 5. Toolchain & hermeticity map

| Component | Codegen | Compile + run | Hermetic? | Remote-cached? |
|---|---|---|---|---|
| **sudoc** | — | `rules_rust` `rust_binary` | ✅ | ✅ |
| **lockstep_diff** | — | `rules_rust` `rust_binary` | ✅ | ✅ |
| **Python** | in-tree | `rules_python` `py_binary` | ✅ | ✅ |
| **JS** | in-tree | `rules_js`/`nodejs` | ✅ | ✅ |
| **C** | in-tree | `hermetic_cc_toolchain` `cc_binary` | ✅ Linux / ⚠️ macOS sysroot | ✅ Linux |
| **Rust** | in-tree | `rules_rust` `rust_binary` | ✅ | ✅ |
| **Zig** | in-tree | `rules_zig` `zig_binary` | ✅ (zig 0.16) | ✅ |
| **Swift** | in-tree | `rules_swift` `swift_binary` | ✅ Linux / ⚠️ macOS SDK | ✅ Linux / ❌ macOS run |
| **Haskell** | `//backends/haskell:emit` (`haskell_binary`) | `rules_haskell` `haskell_binary` | ⚠️ GHC bindist | build ✅ / run no-remote-cache |

**Toolchain corrections baked in (from research):**
- `hermetic_cc_toolchain` covers **C only** — it deliberately does not expose a
  usable Zig-language compiler. **`rules_zig`** (bzlmod-native) provides the Zig
  backend. Two distinct tools.
- **Swift-on-macOS** uses the system Apple SDK (un-vendorable); that run leaf is
  `no-remote-cache`. Linux Swift is hermetic via swift.org's toolchain.
- **`hermetic_cc` on macOS** has incomplete sysroot support; default is hermetic
  C on Linux, system clang for the macOS C run leaf (tagged like Swift). Exact
  call finalized during Phase 1.

**Preserved coverage guarantees:**
- **C sanitizers** (asan/ubsan, +lsan on Linux) — the "C sanitizers: on" CI
  assertion is kept. ⚠️ **Under-verified, spike required before Phase 1b:**
  `hermetic_cc` is zig-cc-based, and zig cc's **ASan/LSan runtime support is
  historically incomplete** — `copts`/`linkopts` sanitizer flags may not link.
  Fallback if the spike fails: a **host-clang sanitizer leaf** (tagged
  `no-remote-cache` like Swift/macOS) even on Linux, which weakens this table's
  hermeticity claim for the sanitizer path specifically. UBSan under zig cc is
  fine; ASan/LSan is the risk.
- **Per-backend flags** (e.g. Haskell `-with-rtsopts=-K8m`) move from JSON into
  the respective Bazel target attributes (`ghcopts`) — nothing lost.
- **The `unsafe_code = "forbid"` workspace lint** is **not** read by rules_rust
  (`[workspace.lints]` is a cargo concept). It must be re-wired explicitly as
  `rustc_flags`/a shared lint attr on the rust targets, or the gate silently
  disappears. Same for any workspace-level clippy config.

## 6. `rules_sudo` public API

**Current state (corrected):** rules_sudo v0.2.1 already ships `sudo_library`,
`sudo_py_library`, `sudo_js_library`, and a PATH-inheriting `sudo_lockstep_test`.
This migration *replaces* `sudo_lockstep_test` with hermetic semantics (breaking
→ rules_sudo 1.0.0, §2.5) and adds the macros below. There is no `sudo_transpile`
today; codegen for one backend is exposed via `sudo_{py,js}_library` and
generalized here.

Public macros (`rules_sudo/defs.bzl`):

```python
sudo_library(name, srcs, deps = [])         # EXISTING — a .sudo module + import graph

sudo_lockstep_test(name, lib, backends = ALL_BACKENDS)   # REPLACED (hermetic)
    # per backend: codegen → build+run (never-fail wrapper) → captured-run file
    # then lockstep_diff(tests-manifest, N captured-run files) → a bazel test target

sudo_external_backend(name, emitter, runner)             # NEW — plugin registration
    # emitter: any executable target (IR-JSON → source)
    # runner : satisfies the backend interface below
```

**The interface contract that makes it uniform:** a *backend* is anything that,
**given generated source for module M, produces M's captured-run file**
(`{stdout, stderr, exit-code}` via a never-fail wrapper — §3). The seven
built-ins are rules_sudo-shipped instances (`sudo_backend_py` wraps `py_binary`
+never-fail-run, `sudo_backend_c` wraps `cc_binary`+run+sanitizers,
`sudo_backend_hs` wraps emitter+`haskell_binary`+run, …). A plugin author's
`sudo_external_backend` is their own instance of the same contract.
`sudo_lockstep_test` fans out over "things that produce captured-run files" and
hands them (plus the tests manifest) to `lockstep_diff` — adding a backend never
touches `sudo_lockstep_test`. (Starlark dispatch — provider-based vs.
macro-valued — is settled in the implementation plan; it is mechanism, not shape.)

**`lockstep_diff` must reproduce the current `render()` divergence ergonomics:**
the cross-target table, Map/Set-iteration-order hints, and the StackOverflow /
sanitizer-crash "no result" annotations (`harness/src/lib.rs`). If it doesn't,
the UX regresses even when correctness holds.

**Consumers** (`conformance/`, `stdlib/`, `examples/`) call `sudo_library` +
`sudo_lockstep_test` per module and roll them into `test_suite`s.

## 7. Cache, CI, and dev UX

**`.bazelrc`:**
```
build --disk_cache=~/.cache/bazel-disk          # local dev fallback
test  --test_output=errors
build:remote --remote_cache=grpcs://remote.buildbuddy.io
build:remote --remote_timeout=600
build:ci --config=remote --remote_upload_local_results
```
- Remote cache: **BuildBuddy free tier** (self-hosted `bazel-remote` is the
  no-third-party alternative). **Not** `actions/cache` — GHA's 10 GB cap thrashes
  on a build this size.
- Auth: CI passes `--remote_header=x-buildbuddy-api-key=$KEY` from a GHA secret
  (bazelrc can't read env, so the header is a CLI flag).
- **Security:** only trusted runs (main-branch pushes) get cache *write*; fork
  PRs get read-only or no key, so a malicious PR can't poison the cache.
- **Wired in Phase 0, not Phase 4** — cargo/Bazel coexistence doubles CI cost for
  the whole migration, so the remote cache is the mitigation and must land first.

**CI restructure:** both jobs become `bazel test --config=ci //...`. Most
toolchain-setup steps vanish (`setup-zig`, `setup-ghc`, `rust-cache`) — Bazel
provisions hermetic toolchains and the remote cache replaces rust-cache. The
macOS runner's Xcode/Swift SDK is the only runner-provided piece. clippy →
`rust_clippy` targets; rustdoc → `rust_doc` targets; the C-sanitizer assertion is
preserved. **Option:** Linux Swift is hermetic, so the Linux job could grow
6→7 backends — offered, not required.

**Dev UX:**
- `bazel run //:gen_rust_project` → `rust-project.json` for rust-analyzer (regen
  on dep changes; pin one compilation mode; expect an occasional `bazel clean` per
  known rules_rust rough edges).
- **`tools/lockstep` — a plain script over `bazel test`**, NOT `bazel run
  //conformance:run` (which would be bazel-invoking-bazel: workspace lock,
  `--script_path` games). The script drives `bazel test` with friendly arg
  handling (`tools/lockstep foo` runs one module) and surfaces the `lockstep_diff`
  divergence table. The "run suite" is a script that drives `bazel test`, not a
  Bazel target that shells Bazel.
- `bazel test //conformance:all` and tag-filtered slices.

## 8. Migration sequencing

cargo and Bazel **coexist until Phase 4**, so CI stays green at every step and
cargo guards correctness until Bazel fully covers all 7 backends. Two ordering
principles from the Fable review: (a) Phase 1 **adds** `lockstep_diff` as a new
binary — it can't "narrow" `harness` while cargo CI still runs `sudoc
conformance` (the cli depends on harness orchestration until Phase 4); the
narrowing/deletion is Phase 4. (b) The remote cache lands in **Phase 0**, not 4,
because coexistence doubles CI cost the whole way.

| Phase | Lands | Gate |
|---|---|---|
| **0** | `MODULE.bazel` + rules_rust + crate_universe; build sudoc + `rust_test` all 14 crates; clippy/rustdoc/gen_rust_project; **`unsafe_code=forbid` re-wired as `rustc_flags`**; **BuildBuddy remote cache wired** | `bazel build //…:sudoc` + tests green (cargo still works); cache hit on rerun |
| **1a** | The **contracts**: `sudoc emit-ir` (JSON boundary artifact) + per-module **tests manifest** + the **never-fail run-wrapper**; **add** `lockstep_diff` (new binary, shares `parse_tap`/canonicalize with harness); rules_python + rules_js; `sudo_library` + `sudo_lockstep_test` for **py/js** (interpreter rules, lowest friction; reuse existing `sudo_py_library` staging) | `bazel test //conformance:all` green, 2 backends |
| **1b** | hermetic_cc (**after the ASan spike, §9**) + rules_zig; add **c/zig/rs** backends | 5 backends |
| **2** | `rules_swift` (Linux hermetic / macOS SDK) | 6 backends |
| **3** | `rules_haskell` — emitter as `haskell_binary`, generated via rules_haskell; **delete `hs.sudoc-backend.json`**; fallback = system-GHC `no-sandbox` leaf if bzlmod fights | 7 backends |
| **4** | **Retire cargo**: delete cargo CI, delete monolithic orchestration (`sudoc conformance`, `sudoc test`, `discovered_backends`, `--external`) — the `harness`→`lockstep_diff` *narrowing* happens here | CI is Bazel-only, green |
| **4.5** | **Matched-pair release**: cut a `sudoc` + `lockstep_diff` release together; extend rules_sudo's extension to fetch **both** assets with a **protocol-version handshake** (`extensions.bzl` fetches one asset today) | Downstream can pin a working (sudoc, lockstep_diff) pair |
| **5** | **rules_sudo 1.0.0** (breaking, §2.5): new hermetic `sudo_lockstep_test`, `sudo_external_backend` + a **minimal** reference example (a shell-script backend — not gold-plated); **migrate infinite-craft-cli's parity tests** to the new API; update `spec/protocol.md` for the IR-JSON boundary | Plugin path documented + demoed; infinite-craft-cli green on 1.0.0 |

## 9. Risks & mitigations

- **Haskell / rules_haskell is the riskiest integration** — community-maintained,
  thinner bzlmod polish, fiddly GHC toolchain, and the emitter itself needs GHC
  (double bootstrap). Isolated to Phase 3 with an explicit fallback (system-GHC
  `no-sandbox` leaf) so it cannot block the migration.
- **rust-analyzer under Bazel-only** is less turnkey than cargo — `gen_rust_project`
  has known config-mode/proc-macro rough edges. Mitigation: a `bazel run` regen
  target, one pinned compilation mode, documented `bazel clean` recovery.
- **hermetic_cc macOS sysroot** incomplete — default to hermetic-C-on-Linux,
  system clang on the macOS C run leaf.
- **ASan/LSan under hermetic_cc (zig cc)** may not link — **spike before Phase 1b**
  (§5). Fallback: host-clang sanitizer leaf tagged `no-remote-cache`. This is a
  bigger practical risk than rust-analyzer and gates the C backend's coverage story.
- **Silent loss of correctness gates** — `[workspace.lints] unsafe_code=forbid`
  and clippy config aren't read by rules_rust; must be re-wired as `rustc_flags`
  / `rust_clippy` targets in Phase 0 or they vanish unnoticed.
- **Release ordering / breaking change** — the new rules_sudo macros need a
  matched `sudoc`+`lockstep_diff` release (dual-asset fetch + protocol handshake,
  Phase 4.5) and a rules_sudo 1.0.0 major bump; infinite-craft-cli is the one
  downstream consumer to migrate (Phase 5).
- **Cache poisoning via fork PRs** — write access gated to trusted runs.

## 10. Out of scope / deferred

- Remote *execution* (RBE) — only remote *cache* is in scope.
- Migrating the release-binary workflow to RBE — it stays a `rust_binary` build
  with platform transitions; wiring is Phase 4/5 detail.
- Any change to language semantics, backends' generated code, or the conformance
  corpus content — this migration changes *how* things build/run, not *what*.

## Success criteria

- `bazel test //...` builds the compiler and runs all-7-backend lockstep
  conformance green, from a clean checkout, on Linux and macOS.
- A one-line change in one backend reruns only that backend's actions (verified
  via `--subcommands`/BEP), demonstrating fine-grained caching.
- Remote cache hit on a second CI run with no source changes.
- `cargo` is fully retired; no `cargo build` anywhere in CI or docs.
- **No correctness gate lost:** `unsafe_code=forbid`, clippy `-D warnings`,
  rustdoc `-D warnings`, and C sanitizers are all enforced under Bazel (verified
  by a deliberately-violating fixture failing the build for each).
- **A crashed runner still surfaces as a divergence,** not a silent Bazel
  failure — the never-fail wrapper + tests-manifest contract holds (verified with
  a StackOverflow/OOB fixture).
- A third-party backend author can register + lockstep-test a new backend in
  their own repo depending only on `rules_sudo` + the released toolchain (the
  minimal reference example builds and passes).
- rust-analyzer works from `bazel run //:gen_rust_project`.
- infinite-craft-cli's parity tests pass on rules_sudo 1.0.0.
