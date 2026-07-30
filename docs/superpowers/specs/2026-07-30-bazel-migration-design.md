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
`sudoc` binary. It currently ships codegen only.

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
   `sudoc`.
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
                        │  each emits a canonical-outcome artifact     own cache keys
                 lockstep_diff (rust_binary, hermetic)               ← the "harness", narrowed
                        │  pure fn of N outcome files → equivalence
        //conformance:<module>_test   +   test_suite //conformance:all
        //conformance:run   (bazel run wrapper for interactive local UX)
```

- **Hermetic & remote-cached:** `sudoc`, every codegen action, `lockstep_diff`,
  and the py/c/js/rs/zig build+run leaves.
- **Non-hermetic residue (tagged `no-remote-cache`):** the Swift-on-macOS run
  leaf and (defensively) the Haskell run leaf. Everything else caches.
- **The Rust `harness` crate narrows** to the `lockstep_diff` tool
  (canonicalize + compare). Bazel's DAG does the orchestration the crate does
  today. This is the single largest piece of work.

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
│   ├── defs.bzl          # sudo_transpile (existing) + sudo_library / sudo_lockstep_test /
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
- **C sanitizers** (asan/ubsan, +lsan on Linux) set via `copts`/`linkopts` on the
  C run leaf; the "C sanitizers: on" CI assertion is kept.
- **Per-backend flags** (e.g. Haskell `-with-rtsopts=-K8m`) move from JSON into
  the respective Bazel target attributes (`ghcopts`) — nothing lost.

## 6. `rules_sudo` public API

Four public macros (`rules_sudo/defs.bzl`):

```python
sudo_library(name, srcs, deps = [])
    # a .sudo module + its import graph; provides SudoLibraryInfo

sudo_transpile(name, lib, backend)          # EXISTING — emit host source for one backend

sudo_lockstep_test(name, lib, backends = ALL_BACKENDS)
    # per backend: codegen → build+run → canonical-outcome file
    # then lockstep_diff over all outcome files → a bazel test target

sudo_external_backend(name, emitter, runner)
    # register a NEW backend for use in sudo_lockstep_test
    # emitter: any executable target (IR-JSON → source)
    # runner : satisfies the backend interface below
```

**The interface contract that makes it uniform:** a *backend* is anything that,
**given generated source for module M, produces M's canonical-outcome file.**
The seven built-ins are rules_sudo-shipped instances (`sudo_backend_py` wraps
`py_binary`+run, `sudo_backend_c` wraps `cc_binary`+run+sanitizers,
`sudo_backend_hs` wraps emitter+`haskell_binary`+run, …). A plugin author's
`sudo_external_backend` is their own instance of the same contract.
`sudo_lockstep_test` fans out over "things that produce outcome files" and diffs
them — adding a backend never touches `sudo_lockstep_test`, and `lockstep_diff`
stays a pure function of N outcome files. (Starlark dispatch mechanism —
provider-based vs. macro-valued — is settled in the implementation plan; it is
mechanism, not shape.)

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
- `bazel run //conformance:run -- --module foo` — interactive lockstep,
  preserving today's feel.
- `bazel test //conformance:all` and tag-filtered slices.

## 8. Migration sequencing

cargo and Bazel **coexist until Phase 4**, so CI stays green at every step and
cargo guards correctness until Bazel fully covers all 7 backends.

| Phase | Lands | Gate |
|---|---|---|
| **0** | `MODULE.bazel` + rules_rust + crate_universe; build sudoc + `rust_test` all 14 crates; clippy/rustdoc/gen_rust_project | `bazel build //…:sudoc` + tests green (cargo still works) |
| **1** | hermetic_cc, rules_zig, rules_python, rules_js; narrow harness→`lockstep_diff`; `sudo_library` + per-backend rules + `sudo_lockstep_test` for **py/c/js/rs/zig** | `bazel test //conformance:all` green, 5 backends |
| **2** | `rules_swift` (Linux hermetic / macOS SDK) | 6 backends |
| **3** | `rules_haskell` — emitter as `haskell_binary`, generated via rules_haskell; **delete `hs.sudoc-backend.json`**; fallback = system-GHC `no-sandbox` leaf if bzlmod fights | 7 backends |
| **4** | **Retire cargo**: delete cargo CI, delete monolithic orchestration (`sudoc conformance`, `sudoc test`, `discovered_backends`, `--external`); wire BuildBuddy in CI | CI is Bazel-only, green |
| **5** | rules_sudo public API polish: `sudo_external_backend` + reference example backend + docs; update `spec/protocol.md` for the IR-JSON boundary | Plugin path documented + demoed |

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
- A third-party backend author can register + lockstep-test a new backend in
  their own repo depending only on `rules_sudo` + the released toolchain (the
  reference example builds and passes).
- rust-analyzer works from `bazel run //:gen_rust_project`.
