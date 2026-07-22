# rules_sudo

Bazel rules for compiling [sudocode](https://github.com/hacker6284/sudocode)
(`.sudo`) sources into generated Python and JavaScript libraries via the
pinned `sudoc` release binary, plus a lockstep test rule.

**v1** — small and honest. No BCR registration yet; consume via
`local_path_override` or `archive_override` against a release tarball.

## Consumer setup

```starlark
# MODULE.bazel
bazel_dep(name = "rules_sudo", version = "0.1.0")

# rules_sudo ships as a release asset starting with the first tag that
# includes this ruleset (naming: rules_sudo-<tag>.tar.gz). Example for a
# future v0.2.0-style tag — do not assume v0.1.0's GitHub release already
# contains the tarball (that release predated the ruleset):
archive_override(
    module_name = "rules_sudo",
    urls = ["https://github.com/hacker6284/sudocode/releases/download/v0.2.0/rules_sudo-v0.2.0.tar.gz"],
    # strip_prefix only if the tarball nests under rules_sudo/ — the release
    # packaging ships the directory as rules_sudo/… so either use
    # strip_prefix = "rules_sudo" or point local_path_override at a checkout.
    strip_prefix = "rules_sudo",
)

# Or, for local development of sudocode itself:
# local_path_override(module_name = "rules_sudo", path = "path/to/sudocode/rules_sudo")

bazel_dep(name = "rules_python", version = "1.7.0")  # if you use sudo_py_library / py_test
bazel_dep(name = "platforms", version = "1.0.0")
bazel_dep(name = "bazel_skylib", version = "1.7.1")

# Bind @sudo_toolchain//:sudoc in *this* module's repo mapping.
# rules_sudo also self-invokes the same extension so Label defaults inside
# private/rules.bzl resolve; both bindings share one globally computed repo.
sudo = use_extension("@rules_sudo//:extensions.bzl", "sudo")
sudo.toolchain(version = "v0.1.0")
use_repo(sudo, "sudo_toolchain")
```

## Rule usage

```starlark
load("@rules_sudo//:defs.bzl", "sudo_library", "sudo_py_library", "sudo_js_library", "sudo_lockstep_test")

sudo_library(
    name = "shout",
    srcs = ["shout.sudo"],
)

sudo_library(
    name = "greet",
    srcs = ["greet.sudo"],
    deps = [":shout"],  # file-import graph; import std.* needs no dep
)

sudo_py_library(
    name = "greet_py",
    lib = ":greet",
    entry = "greet.sudo",
)

sudo_js_library(
    name = "greet_js",
    lib = ":greet",
    entry = "greet.sudo",
)

sudo_lockstep_test(
    name = "greet_lockstep_test",
    lib = ":greet",
    entry = "greet.sudo",
    targets = ["py", "js"],
)
```

`sudo_py_library` / `sudo_js_library` emit a **tree artifact** (directory)
of generated files — never enumerate those names in BUILD files. The Python
package import path is the target name (e.g. `from greet_py import greet`).

## Local dev override

```starlark
sudo = use_extension("@rules_sudo//:extensions.bzl", "sudo")
sudo.local_binary(path = "/abs/path/to/sudoc")
use_repo(sudo, "sudo_toolchain")
```

**Precedence:** if any module in the dependency graph declares
`sudo.local_binary(path = ...)`, that absolute path wins unconditionally —
no release binaries are downloaded. Useful when iterating on `sudoc` itself.

## Toolchain shipping shape

v1 ships a **hub alias** `@sudo_toolchain//:sudoc`: a module extension
fetches (or overrides) platform binaries and a small `repository_rule`
exposes a `select()`-based alias over `config_setting`s for
macos_arm64 / linux_x86_64 / linux_aarch64.

This is **not** full `toolchain_type` / `register_toolchains` resolution.
For a single binary per platform with no multi-version or multi-vendor
matrix, the hub alias is enough and avoids the ceremony of custom
toolchain types. Revisit if BCR registration or multi-version coexistence
becomes real.

**v1 limitation:** if two modules declare `sudo.toolchain(version = ...)`
with different versions, last-write-wins over `module_ctx.modules` visit
order (not guaranteed stable on complex graphs). Fine until BCR multi-
version consumers appear.

## v1 non-goals

- No BCR registration (use `local_path_override` / `archive_override` only)
- No capture rules of any kind
- `sudo_lockstep_test` always gets `tags = ["local"]` and inherits `PATH`
  (host `python3` / `node` dependency; not sandboxed)
- No Windows platform pins yet

## Layout

| Path | Role |
|------|------|
| `defs.bzl` | Public API — only file consumers should `load()` |
| `extensions.bzl` | `sudo` module extension (toolchain / local_binary) |
| `versions.bzl` | Pinned `sudoc` release sha256s |
| `private/rules.bzl` | Rule implementations |
| `e2e/` | Standalone module proving the rules end-to-end |

## Updating toolchain pins

When cutting a new sudocode release, add a version key to
`SUDO_TOOLCHAIN_VERSIONS` in `versions.bzl` with fresh sha256s from the
GitHub release assets (verify before pinning).

## Pinning a newer sudoc via `sha256s`

If you need a `sudoc` release newer than the pins shipped in this ruleset's
`versions.bzl` (without waiting for a rules_sudo release that updates the
manifest), pass `sha256s` on `sudo.toolchain`. Keys are release-asset
**triples** (the same strings as `PLATFORM_TRIPLES` values in
`versions.bzl`), not the internal platform keys:

```starlark
sudo = use_extension("@rules_sudo//:extensions.bzl", "sudo")
sudo.toolchain(
    version = "v0.2.0",
    sha256s = {
        "aarch64-apple-darwin": "<sha256>",
        "x86_64-unknown-linux-gnu": "<sha256>",
        "aarch64-unknown-linux-gnu": "<sha256>",
    },
)
use_repo(sudo, "sudo_toolchain")
```

For each platform, an entry in `sha256s` overrides the versions.bzl pin;
triples not listed still fall back to the manifest for `version`.
