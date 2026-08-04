"""Manifest of pinned sudoc release binaries, by version, asset, and platform.

Phase 5 (rules_sudo 1.0.0): the hermetic decomposed `sudo_lockstep_test` needs a
MATCHED SET of first-party binaries — `sudoc` (emits codegen / recipes /
tests-manifest) plus the harness tools `lockstep_diff`, `capture_run`,
`emit_unpack` (consume them). A release publishes the set built from one commit;
the run-time protocol handshake (`sudoc protocol-version` vs
`lockstep_diff --protocol-version`) rejects a mispaired toolchain.

`sudoc` was the only asset published by the pre-1.0 releases; v0.1.0 pins it.
The matched-pair release v0.3.0 (design §8 Phase 5 Task 8) publishes the whole
set — `sudoc` + `lockstep_diff` + `capture_run` + `emit_unpack` — built from one
commit.

CHICKEN-AND-EGG (Fable's guidance, design §8 Phase 4.5): the rules_sudo tarball
shipped IN release v0.3.0 must carry v0.3.0's own binary sha256s, but those only
exist AFTER the binaries build. So the v0.3.0 slots below ship EMPTY in the repo;
`.github/workflows/release.yml` builds each asset `-c opt` per platform, computes
its sha256, and REGENERATES the `_V0_3_0` block (between the INJECT markers) via
`tools/inject_release_shas.py` BEFORE packaging the tarball. Do NOT hand-edit the
v0.3.0 shas — they come from the workflow.

Update when cutting a NON-workflow release, or to re-pin by hand from a published
release: for each published asset add a per-platform sha256. Fetch the `.sha256`
assets fresh from the GitHub release (don't trust a stale copy) and verify before
pinning, e.g.:

    gh release download <tag> --repo hacker6284/sudocode --pattern '*.sha256'
    gh release download <tag> --repo hacker6284/sudocode --pattern 'sudoc-*' --clobber
    shasum -a 256 -c sudoc-<triple>.sha256
"""

# platform_target key -> release asset target-triple suffix. Release assets are
# named `<asset>-<triple>` / `<asset>-<triple>.sha256`.
PLATFORM_TRIPLES = {
    "macos_arm64": "aarch64-apple-darwin",
    "linux_x86_64": "x86_64-unknown-linux-gnu",
    "linux_aarch64": "aarch64-unknown-linux-gnu",
}

# The matched set a release publishes. `sudoc` is mandatory; the harness tools
# are present only once the matched-pair release ships them (Task 8).
RELEASE_ASSETS = ["sudoc", "lockstep_diff", "capture_run", "emit_unpack"]

# ── v0.3.0 sha256 INJECTION POINT ────────────────────────────────────────────
# The matched-pair release (design §8 Phase 5 Task 8) publishes all four assets
# built from ONE commit. Because the rules_sudo tarball shipped inside release
# v0.3.0 must carry that release's own binary sha256s — which only exist after
# the binaries build — these slots ship EMPTY in the repo. release.yml builds
# each asset `-c opt` per platform, computes its sha256, and REGENERATES the
# block between the INJECT-v0.3.0 markers below (via tools/inject_release_shas.py)
# BEFORE packaging the tarball. Do NOT hand-edit these shas — keep the markers.
#
# In-repo (empty) state is harmless: nothing here resolves v0.3.0 in release
# mode — sudocode and the reference example drive the toolchain via
# `sudo.local_binary(...)` (HEAD dogfood), and rules_sudo self-invokes at v0.1.0.
# INJECT-v0.3.0-BEGIN
_V0_3_0 = {
    "sudoc": {
        "macos_arm64": "",
        "linux_x86_64": "",
        "linux_aarch64": "",
    },
    "lockstep_diff": {
        "macos_arm64": "",
        "linux_x86_64": "",
        "linux_aarch64": "",
    },
    "capture_run": {
        "macos_arm64": "",
        "linux_x86_64": "",
        "linux_aarch64": "",
    },
    "emit_unpack": {
        "macos_arm64": "",
        "linux_x86_64": "",
        "linux_aarch64": "",
    },
}
# INJECT-v0.3.0-END

# An asset→platform sha of "" means "not pinned" (identical to an absent key):
# `_prune_empty` drops empties so the module extension's fail()/skip logic is
# unchanged — an empty `sudoc` slot fails loudly, an empty optional asset is
# simply not exposed. Full slots ship only after the workflow injects them.
def _prune_empty(version_manifest):
    out = {}
    for asset, plats in version_manifest.items():
        kept = {p: sha for p, sha in plats.items() if sha}
        if kept:
            out[asset] = kept
    return out

# version -> asset -> platform_target -> sha256
SUDO_TOOLCHAIN_VERSIONS = {
    "v0.3.0": _prune_empty(_V0_3_0),
    "v0.1.0": {
        "sudoc": {
            "macos_arm64": "0829935f9a68a142b6179f58c84508cb9d07c7b08be6253c653677e7a991806b",
            "linux_x86_64": "3343d00da2d6a816671611d0c10b72630e0ad0e5c192975ed47b9ccce5834e94",
            "linux_aarch64": "01906a8354101a6e4cc2b1804e8e6e5862c774ed7813a6332643637d5eb98b07",
        },
    },
}
