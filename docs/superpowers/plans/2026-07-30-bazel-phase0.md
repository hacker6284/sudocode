# Bazel Migration — Phase 0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bazel builds and tests the `sudoc` Rust compiler alongside cargo, with the remote cache and every correctness gate (unsafe_code, clippy, rustdoc) wired — the foundation all later phases build on.

**Architecture:** bzlmod root module + `rules_rust` + `crate_universe` (3 external crates: serde, serde_json, schemars). Per-crate `rust_library`/`rust_binary`/`rust_test`. cargo is untouched and still works; Bazel is *added alongside*. No backend toolchains yet — only Rust.

**Tech Stack:** Bazel 9.2 (bazelisk), rules_rust, crate_universe, BuildBuddy remote cache.

## Global Constraints

- **Bazel-only is the *end* goal (Phase 4); in Phase 0 cargo and Bazel coexist** — do not delete any cargo file, and `cargo build`/`cargo test` must still pass after every task.
- **Keep `Cargo.toml`/`Cargo.lock`** — `crate_universe` reads `Cargo.lock` as the dependency source of truth.
- **`unsafe_code = "forbid"`** — the workspace lint is NOT read by rules_rust; it MUST be re-wired as an explicit `rustc_flags`/`rustc_env` gate, or it silently vanishes.
- **No backend toolchains in Phase 0** — do not add hermetic_cc/rules_zig/rules_swift/rules_haskell/rules_python/rules_js yet. Integration tests that shell out to language toolchains (`execute.rs`, `sanitizer.rs`, `boundary.rs`, `adapter.rs`, `lockstep.rs`, `std_equivalence.rs`, `discovery*.rs`, `search_path.rs`) are OUT of scope for Phase 0's `bazel test` and stay cargo-only until their toolchain lands.
- **Repo layout:** root module at repo root; the Cargo workspace lives under `sudoc/`; repo-root `stdlib/*.sudo` are `compile_data` for the `sudoc-stdlib` crate.

**Crate dependency graph (verified from Cargo.toml):**
```
syntax    → (ext: serde, schemars)
ir        → syntax          (ext: serde, serde_json, schemars)
sdk       → ir
stdlib    → ()              (compile_data: //stdlib:*.sudo)
types     → syntax, ir, stdlib
backend_py/c/js/swift/rs/zig → ir, sdk, types   (compile_data: src/runtime/*)
backend_ext → ir, sdk, types  (ext: serde, serde_json)
harness   → ir, sdk, types, backend_{py,c,js,swift,rs,zig,ext}
cli       → syntax, types, ir, backend_py, backend_c, backend_ext, harness
```

**Non-shelling tests to run under Bazel in Phase 0** (the rest stay cargo-only):
`syntax`: lexer.rs, parser.rs · `types`: checker.rs, golden.rs, imports.rs ·
`ir`: roundtrip.rs, schema_golden.rs · `backend_{py,js,rs,swift}`: emit.rs ·
`harness`: wire_trip.rs, sanitizer_classification.rs · plus every crate's `#[cfg(test)]` unit tests (via `rust_test` with `crate = ":lib"`).

---

### Task 1: Root module + rules_rust + crate_universe; first leaf crate builds

**Files:**
- Create: `MODULE.bazel`, `.bazelrc`, `BUILD.bazel`, `sudoc/crates/syntax/BUILD.bazel`
- Create: `sudoc/crates/BUILD.bazel` (empty package marker if needed)

**Interfaces:**
- Produces: `@crates//:serde`, `@crates//:serde_json`, `@crates//:schemars` (crate_universe repo); `//sudoc/crates/syntax:syntax` (rust_library).

- [ ] **Step 1: Write `MODULE.bazel`**

```python
module(name = "sudocode", version = "0.1.0")

bazel_dep(name = "rules_rust", version = "0.59.1")

rust = use_extension("@rules_rust//rust:extensions.bzl", "rust")
rust.toolchain(edition = "2021", versions = ["1.85.0"])
use_repo(rust, "rust_toolchains")
register_toolchains("@rust_toolchains//:all")

crate = use_extension("@rules_rust//crate_universe:extensions.bzl", "crate")
crate.from_cargo(
    name = "crates",
    cargo_lockfile = "//sudoc:Cargo.lock",
    manifests = [
        "//sudoc:Cargo.toml",
        "//sudoc/crates/syntax:Cargo.toml",
        "//sudoc/crates/types:Cargo.toml",
        "//sudoc/crates/ir:Cargo.toml",
        "//sudoc/crates/sdk:Cargo.toml",
        "//sudoc/crates/stdlib:Cargo.toml",
        "//sudoc/crates/backend_py:Cargo.toml",
        "//sudoc/crates/backend_c:Cargo.toml",
        "//sudoc/crates/backend_js:Cargo.toml",
        "//sudoc/crates/backend_swift:Cargo.toml",
        "//sudoc/crates/backend_rs:Cargo.toml",
        "//sudoc/crates/backend_zig:Cargo.toml",
        "//sudoc/crates/backend_ext:Cargo.toml",
        "//sudoc/crates/harness:Cargo.toml",
        "//sudoc/crates/cli:Cargo.toml",
    ],
)
use_repo(crate, "crates")
```

Note: verify `rules_rust` latest release and the pinned rustc version as Step-2 shows; adjust the two version strings if the fetch reports a newer stable.

- [ ] **Step 2: Write `.bazelrc` (cache + lint scaffolding, remote added in Task 8)**

```
common --enable_bzlmod
build --@rules_rust//rust/settings:extra_rustc_flags=-Dunsafe_code
test --test_output=errors
build --disk_cache=~/.cache/bazel-disk
```

- [ ] **Step 3: Write root `BUILD.bazel`** (empty package; targets added later)

```python
# Root package. gen_rust_project alias + top-level suites added in later tasks.
```

- [ ] **Step 4: Write `sudoc/crates/syntax/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "syntax",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_syntax",
    deps = [
        "@crates//:serde",
        "@crates//:schemars",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 5: Build it (verifies MODULE + crate_universe + toolchain resolve)**

Run: `cd /Users/zachmills/Documents/Projects/sudocode && bazel build //sudoc/crates/syntax:syntax`
Expected: downloads rules_rust + rustc + fetches serde/schemars, then `BUILD SUCCESSFUL`. If crate_universe complains about a missing manifest, confirm every crate's `Cargo.toml` path in `from_cargo.manifests`.

- [ ] **Step 6: Confirm cargo still works**

Run: `cd sudoc && cargo build -p sudoc-syntax 2>&1 | tail -1`
Expected: `Finished`.

- [ ] **Step 7: Commit**

```bash
git add MODULE.bazel .bazelrc BUILD.bazel sudoc/crates/syntax/BUILD.bazel MODULE.bazel.lock
git commit -m "bazel(phase0): root module + rules_rust + crate_universe; syntax crate builds"
```

---

### Task 2: Remaining pure-Rust crates (ir, sdk, stdlib, types) build

**Files:**
- Create: `sudoc/crates/ir/BUILD.bazel`, `sudoc/crates/sdk/BUILD.bazel`, `sudoc/crates/stdlib/BUILD.bazel`, `sudoc/crates/types/BUILD.bazel`
- Create: `stdlib/BUILD.bazel` (export the repo-root .sudo files as data)

**Interfaces:**
- Consumes: `//sudoc/crates/syntax:syntax`.
- Produces: `//sudoc/crates/{ir,sdk,stdlib,types}` rust_libraries; `//stdlib:sudo_srcs` filegroup.

- [ ] **Step 1: Write `stdlib/BUILD.bazel`** (the cross-package data the stdlib crate needs)

```python
filegroup(
    name = "sudo_srcs",
    srcs = glob(["*.sudo"]),
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 2: Write `sudoc/crates/ir/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "ir",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_ir",
    deps = [
        "//sudoc/crates/syntax:syntax",
        "@crates//:serde",
        "@crates//:serde_json",
        "@crates//:schemars",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 3: Write `sudoc/crates/sdk/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "sdk",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_sdk",
    deps = ["//sudoc/crates/ir:ir"],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 4: Write `sudoc/crates/stdlib/BUILD.bazel`** (compile_data reaches repo-root stdlib)

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "stdlib",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_stdlib",
    # include_str!("../../../../stdlib/*.sudo") — the sandbox preserves repo-relative
    # layout, so these must be present as compile_data at their repo path.
    compile_data = ["//stdlib:sudo_srcs"],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 5: Write `sudoc/crates/types/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "types",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_types",
    deps = [
        "//sudoc/crates/syntax:syntax",
        "//sudoc/crates/ir:ir",
        "//sudoc/crates/stdlib:stdlib",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 6: Build all four**

Run: `bazel build //sudoc/crates/ir //sudoc/crates/sdk //sudoc/crates/stdlib //sudoc/crates/types`
Expected: `BUILD SUCCESSFUL`. If stdlib fails with `couldn't read file ../../../../stdlib/regex.sudo`, the `compile_data`/`//stdlib:sudo_srcs` wiring is wrong — the file must appear in the sandbox at repo-root `stdlib/`.

- [ ] **Step 7: Commit**

```bash
git add sudoc/crates/{ir,sdk,stdlib,types}/BUILD.bazel stdlib/BUILD.bazel
git commit -m "bazel(phase0): ir/sdk/stdlib/types crates build (stdlib cross-package compile_data)"
```

---

### Task 3: Backend crates build (compile_data for runtime files)

**Files:**
- Create: `sudoc/crates/backend_{py,c,js,swift,rs,zig,ext}/BUILD.bazel`

**Interfaces:**
- Consumes: `//sudoc/crates/{ir,sdk,types}`.
- Produces: `//sudoc/crates/backend_*` rust_libraries.

- [ ] **Step 1: Write `sudoc/crates/backend_py/BUILD.bazel`** (pattern for py/js/swift/rs/zig)

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "backend_py",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_backend_py",
    # include_str!("runtime/_sudo_rt.py")
    compile_data = glob(["src/runtime/**"]),
    deps = [
        "//sudoc/crates/ir:ir",
        "//sudoc/crates/sdk:sdk",
        "//sudoc/crates/types:types",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 2: Write the analogous BUILD.bazel for backend_c, backend_js, backend_swift, backend_rs, backend_zig**

Each is identical to Step 1 with the crate dir + `crate_name` changed (`sudoc_backend_c`, …). `compile_data = glob(["src/runtime/**"])` covers `sudo_rt.{h,c,mjs,swift,rs,zig}` in each. (backend_c's runtime has two files — the glob covers both.)

- [ ] **Step 3: Write `sudoc/crates/backend_ext/BUILD.bazel`** (adds serde; no runtime dir)

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "backend_ext",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_backend_ext",
    deps = [
        "//sudoc/crates/ir:ir",
        "//sudoc/crates/sdk:sdk",
        "//sudoc/crates/types:types",
        "@crates//:serde",
        "@crates//:serde_json",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 4: Build all seven backends**

Run: `bazel build //sudoc/crates/backend_py //sudoc/crates/backend_c //sudoc/crates/backend_js //sudoc/crates/backend_swift //sudoc/crates/backend_rs //sudoc/crates/backend_zig //sudoc/crates/backend_ext`
Expected: `BUILD SUCCESSFUL`. A `couldn't read file runtime/...` error means the `compile_data` glob missed that crate's runtime dir.

- [ ] **Step 5: Commit**

```bash
git add sudoc/crates/backend_*/BUILD.bazel
git commit -m "bazel(phase0): seven backend crates build (runtime compile_data)"
```

---

### Task 4: harness + cli; the sudoc binary builds under Bazel

**Files:**
- Create: `sudoc/crates/harness/BUILD.bazel`, `sudoc/crates/cli/BUILD.bazel`

**Interfaces:**
- Consumes: all backend + core crates.
- Produces: `//sudoc/crates/harness:harness`, `//sudoc/crates/cli:sudoc` (rust_binary).

- [ ] **Step 1: Write `sudoc/crates/harness/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
    name = "harness",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    crate_name = "sudoc_harness",
    deps = [
        "//sudoc/crates/ir:ir",
        "//sudoc/crates/sdk:sdk",
        "//sudoc/crates/types:types",
        "//sudoc/crates/backend_py:backend_py",
        "//sudoc/crates/backend_c:backend_c",
        "//sudoc/crates/backend_js:backend_js",
        "//sudoc/crates/backend_swift:backend_swift",
        "//sudoc/crates/backend_rs:backend_rs",
        "//sudoc/crates/backend_zig:backend_zig",
        "//sudoc/crates/backend_ext:backend_ext",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 2: Write `sudoc/crates/cli/BUILD.bazel`**

```python
load("@rules_rust//rust:defs.bzl", "rust_binary")

rust_binary(
    name = "sudoc",
    srcs = glob(["src/**/*.rs"]),
    edition = "2021",
    deps = [
        "//sudoc/crates/syntax:syntax",
        "//sudoc/crates/types:types",
        "//sudoc/crates/ir:ir",
        "//sudoc/crates/backend_py:backend_py",
        "//sudoc/crates/backend_c:backend_c",
        "//sudoc/crates/backend_ext:backend_ext",
        "//sudoc/crates/harness:harness",
    ],
    visibility = ["//visibility:public"],
)
```

- [ ] **Step 3: Build the binary**

Run: `bazel build //sudoc/crates/cli:sudoc`
Expected: `BUILD SUCCESSFUL`.

- [ ] **Step 4: Smoke-test the built binary matches cargo's behavior**

Run: `bazel run //sudoc/crates/cli:sudoc -- check conformance/semantics/floats.sudo`
Expected: `conformance/semantics/floats.sudo: ok` (proves the Bazel-built sudoc runs and resolves the embedded stdlib).

- [ ] **Step 5: Commit**

```bash
git add sudoc/crates/harness/BUILD.bazel sudoc/crates/cli/BUILD.bazel
git commit -m "bazel(phase0): harness + cli; //sudoc/crates/cli:sudoc builds and runs"
```

---

### Task 5: rust_test targets (non-shelling tests only)

**Files:**
- Modify: `sudoc/crates/{syntax,ir,types}/BUILD.bazel`, `sudoc/crates/backend_{py,js,rs,swift}/BUILD.bazel`, `sudoc/crates/harness/BUILD.bazel` (append test targets)

**Interfaces:**
- Produces: `rust_test` targets runnable via `bazel test`.

- [ ] **Step 1: Add unit-test + integration-test targets to `syntax/BUILD.bazel`**

Append:
```python
load("@rules_rust//rust:defs.bzl", "rust_test")

rust_test(
    name = "syntax_unit",
    crate = ":syntax",
    edition = "2021",
)

[rust_test(
    name = "syntax_" + t,
    srcs = ["tests/" + t + ".rs"],
    edition = "2021",
    deps = [":syntax"],
) for t in ["lexer", "parser"]]
```

- [ ] **Step 2: Add test targets to `ir/BUILD.bazel`**

Append `rust_test(name="ir_unit", crate=":ir")` plus integration tests `roundtrip`, `schema_golden` (srcs `tests/<t>.rs`, deps `[":ir", "@crates//:serde_json"]` — match each test file's `use` lines; add serde_json only if the test references it).

- [ ] **Step 3: Add test targets to `types/BUILD.bazel`**

Append `rust_test(name="types_unit", crate=":types")` plus `checker`, `golden`, `imports` (deps `[":types", "//sudoc/crates/ir:ir"]`; `imports` also writes temp files — no extra dep). These are the F8/F10/F13 checker tests — they must pass.

- [ ] **Step 4: Add `emit` test to backend_{py,js,rs,swift}/BUILD.bazel**

For each, append:
```python
load("@rules_rust//rust:defs.bzl", "rust_test")

rust_test(
    name = "emit",
    srcs = ["tests/emit.rs"],
    edition = "2021",
    deps = [":backend_py", "//sudoc/crates/ir:ir"],  # adjust crate label per backend
)
```
(emit.rs asserts on generated *text* — no toolchain shell-out. `execute.rs`/`boundary.rs`/`sanitizer.rs` are deferred — do NOT add them.)

- [ ] **Step 5: Add `wire_trip` + `sanitizer_classification` to harness/BUILD.bazel**

Append `rust_test`s for `tests/wire_trip.rs` and `tests/sanitizer_classification.rs` (deps `[":harness"]`). These parse/classify strings — no shell-out. `discovery.rs`, `lockstep.rs` are deferred.

- [ ] **Step 6: Run the Bazel test suite**

Run: `bazel test //sudoc/crates/...`
Expected: all added `rust_test` targets PASS. If a test fails "couldn't find file" it likely reads a testdata path — add it to the test's `data` attr.

- [ ] **Step 7: Confirm cargo tests still pass**

Run: `cd sudoc && cargo test --workspace 2>&1 | grep -E "test result: FAILED" || echo "cargo green"`
Expected: `cargo green`.

- [ ] **Step 8: Commit**

```bash
git add sudoc/crates/*/BUILD.bazel
git commit -m "bazel(phase0): rust_test targets for non-shelling tests (unit + emit + checker + wire)"
```

---

### Task 6: Correctness gates — unsafe_code, clippy, rustdoc

**Files:**
- Modify: `.bazelrc`
- Create: `sudoc/crates/cli/BUILD.bazel` clippy/doc entries (or a root aggregation)

**Interfaces:**
- Produces: a clippy invocation and a rustdoc invocation that fail on warnings, matching current CI.

- [ ] **Step 1: Verify the `unsafe_code=forbid` gate actually fires**

Temporarily add `unsafe {}` in a leaf: append to `sudoc/crates/syntax/src/lib.rs` a `fn _u(){ unsafe {} }`.
Run: `bazel build //sudoc/crates/syntax:syntax`
Expected: **FAIL** with `usage of an `unsafe` block` / `deny(unsafe_code)` — proving the `.bazelrc` `-Dunsafe_code` flag from Task 1 is live. Then revert the edit and rebuild → SUCCESS.

- [ ] **Step 2: Wire clippy via the rules_rust aspect in `.bazelrc`**

Append:
```
build:clippy --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect
build:clippy --output_groups=+clippy_checks
build:clippy --@rules_rust//rust/settings:clippy_flags=-Dwarnings
```

- [ ] **Step 3: Run clippy across the workspace**

Run: `bazel build --config=clippy //sudoc/crates/...`
Expected: `BUILD SUCCESSFUL` with zero clippy diagnostics (the tree is already clippy-clean from the CI-fix commit `89d3e75`).

- [ ] **Step 4: Wire rustdoc**

Add to `cli/BUILD.bazel`:
```python
load("@rules_rust//rust:defs.bzl", "rust_doc")

rust_doc(
    name = "sudoc_doc",
    crate = ":sudoc",
    rustdoc_flags = ["-Dwarnings"],
)
```

- [ ] **Step 5: Build docs**

Run: `bazel build //sudoc/crates/cli:sudoc_doc`
Expected: `BUILD SUCCESSFUL`.

- [ ] **Step 6: Commit**

```bash
git add .bazelrc sudoc/crates/cli/BUILD.bazel
git commit -m "bazel(phase0): correctness gates — unsafe_code deny, clippy -Dwarnings, rustdoc -Dwarnings"
```

---

### Task 7: rust-analyzer support (gen_rust_project)

**Files:**
- Modify: root `BUILD.bazel`

**Interfaces:**
- Produces: `//:gen_rust_project` (regenerates `rust-project.json`).

- [ ] **Step 1: Add the gen_rust_project alias to root `BUILD.bazel`**

```python
alias(
    name = "gen_rust_project",
    actual = "@rules_rust//tools/rust_analyzer:gen_rust_project",
)
```

- [ ] **Step 2: Generate the project file**

Run: `bazel run //:gen_rust_project`
Expected: writes `rust-project.json` at repo root; `BUILD SUCCESSFUL`. If it errors on a compilation-mode mismatch, re-run after `bazel clean` (documented rules_rust rough edge).

- [ ] **Step 3: Gitignore the generated file**

Append `rust-project.json` to `.gitignore` (it's machine-generated, per-checkout).

- [ ] **Step 4: Commit**

```bash
git add BUILD.bazel .gitignore
git commit -m "bazel(phase0): //:gen_rust_project for rust-analyzer"
```

---

### Task 8: BuildBuddy remote cache

**Files:**
- Modify: `.bazelrc`
- (CI wiring is noted but lands with the Phase-0 CI job; see Step 4.)

**Interfaces:**
- Produces: `--config=remote` / `--config=ci` cache configs.

- [ ] **Step 1: Add remote-cache configs to `.bazelrc`**

Append:
```
build:remote --remote_cache=grpcs://remote.buildbuddy.io
build:remote --remote_timeout=600
build:ci --config=remote --remote_upload_local_results
build:ci --config=clippy
```

- [ ] **Step 2: Verify a local cache round-trip (disk cache, no key needed)**

Run: `bazel clean && bazel build //sudoc/crates/cli:sudoc && bazel clean && bazel build //sudoc/crates/cli:sudoc 2>&1 | grep -E "remote cache hit|disk cache hit|processes"`
Expected: the second build reports cached actions (`disk cache hit` / far fewer processes), proving the `--disk_cache` path works. (Remote/BuildBuddy key is exercised in CI, not locally.)

- [ ] **Step 3: Document the CI secret + fork-PR gating**

Create `docs/bazel-remote-cache.md` with: the `BUILDBUDDY_API_KEY` GHA secret, the CI flag `--remote_header=x-buildbuddy-api-key=$KEY`, and the rule that only main-branch pushes get cache *write* (fork PRs read-only) to prevent poisoning.

- [ ] **Step 4: Add a Bazel CI job (does not replace the cargo jobs yet)**

Add a new job to `.github/workflows/ci.yml` named "Bazel build+test" that: checks out, installs bazelisk, and runs `bazel test --config=ci //sudoc/crates/... --remote_header=x-buildbuddy-api-key=${{ secrets.BUILDBUDDY_API_KEY }}` (guarded so forks without the secret fall back to `--config=clippy` local-only). The existing macOS/Linux cargo jobs stay untouched.

- [ ] **Step 5: Commit**

```bash
git add .bazelrc docs/bazel-remote-cache.md .github/workflows/ci.yml
git commit -m "bazel(phase0): BuildBuddy remote cache config + Bazel CI job (alongside cargo)"
```

---

## Phase 0 exit gate

- `bazel build //sudoc/crates/cli:sudoc` succeeds; the binary runs `check`/`build` correctly.
- `bazel test //sudoc/crates/...` green (non-shelling tests).
- `bazel build --config=clippy //sudoc/crates/...` clean; `unsafe_code` deny verified to fire; rustdoc `-Dwarnings` clean.
- `cargo build`/`cargo test --workspace` still green (coexistence intact).
- Second build hits the cache.
- `bazel run //:gen_rust_project` produces a working `rust-project.json`.
- CI has a Bazel job alongside the cargo jobs, both green.

## Self-review notes

- **Spec coverage:** Phase 0 row of §8 + the Phase-0 items in §5 (unsafe_code re-wire), §7 (remote cache in Phase 0, gen_rust_project), §9 (rust-analyzer rough edges). ✓
- **Deferred by design:** all backend toolchains, the lockstep decomposition, `emit-ir`, and shelling integration tests — those are Phase 1a+.
- **Version pins to verify at execution:** `rules_rust` release, pinned rustc (`1.85.0`), and the `rust_clippy_aspect` load path — confirm against the installed `rules_rust` before relying on the exact strings (rules_rust's aspect label has moved across versions).
