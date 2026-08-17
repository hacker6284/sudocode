"""Manifest of pinned sudoc release binaries, by version, asset, and platform.

Phase 5 (rules_sudo 1.0.0): the hermetic decomposed `sudo_lockstep_test` needs a
MATCHED SET of first-party binaries — `sudoc` (emits codegen / recipes /
tests-manifest) plus the harness tools `lockstep_diff`, `capture_run`,
`emit_unpack` (consume them). A release publishes the set built from one commit;
the run-time protocol handshake (`sudoc protocol-version` vs
`lockstep_diff --protocol-version`) rejects a mispaired toolchain.

`sudoc` was the only asset published by the pre-1.0 releases; v0.1.0 pins it.
The matched-pair release v0.3.0 (design §8 Phase 5 Task 8) published the whole
set — `sudoc` + `lockstep_diff` + `capture_run` + `emit_unpack` — built from one
commit; those shas are backfilled below so repo state can resolve that tag.

CHICKEN-AND-EGG (Fable's guidance, design §8 Phase 4.5): the rules_sudo tarball
shipped IN a release must carry that release's own binary sha256s, but those only
exist AFTER the binaries build. The PENDING block below names the NEXT planned
tag with empty shas; `.github/workflows/release.yml` builds each asset `-c opt`
per platform, computes its sha256, and REGENERATES the `_PENDING` block
(between the generic INJECT-RELEASE markers) via `tools/inject_release_shas.py
<versions.bzl> <sha_dir> <tag>` BEFORE packaging the tarball. The workflow's tag
argument is what actually gets injected (it need not match the in-repo
placeholder). Do NOT hand-edit the pending shas — they come from the workflow.

Empty slots are pruned by `_prune_empty`, so resolving the pending version from
repo state alone fails loudly — same semantics as before, just no longer locked
to one version number.

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

# ── PENDING release sha256 INJECTION POINT ───────────────────────────────────
# The next matched-pair release publishes all four assets built from ONE commit.
# Because the rules_sudo tarball shipped inside that release must carry that
# release's own binary sha256s — which only exist after the binaries build —
# these slots ship EMPTY in the repo (the version string names the next planned
# tag; empty shas mean nothing resolves it from repo state alone). release.yml
# builds each asset `-c opt` per platform, computes its sha256, and REGENERATES
# the block between the INJECT-RELEASE markers below (via
# tools/inject_release_shas.py, which takes the tag on the command line)
# BEFORE packaging the tarball. The workflow's tag argument is authoritative —
# it need not match `_PENDING_VERSION` here. Do NOT hand-edit these shas —
# keep the markers; the inject script rewrites the whole block.
#
# In-repo (empty) state is harmless: nothing here resolves the pending version in
# release mode — sudocode and the reference example drive the toolchain via
# `sudo.local_binary(...)` (HEAD dogfood), and rules_sudo self-invokes at a
# fully-pinned prior version (v0.3.0 / v0.1.0).
# INJECT-RELEASE-BEGIN
_PENDING_VERSION = "v0.7.3"
_PENDING = {
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
# INJECT-RELEASE-END

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
    _PENDING_VERSION: _prune_empty(_PENDING),
    "v0.7.1": {
        "sudoc": {
            "macos_arm64": "f2317d547abc2887b88822b6309f9cb62aa7ec70310969b41a2b56622364baa0",
            "linux_x86_64": "d71487647fd05131e28432389319158348cb2a710ba4321a8bfc1ed57c6333d4",
            "linux_aarch64": "ce04c6602da5883b1f240f93464f78b50a352527bed5c2dfa3fa4960f61d456f",
        },
        "lockstep_diff": {
            "macos_arm64": "871c9ac870cc8741854c3a191f1021afb48b6e6a7c93f0f77b5f24ba68f933ba",
            "linux_x86_64": "0ad04adc5265af31fa5a1b6568d91f5ddf76d9e59b4f05347e41aac2fceb8fc8",
            "linux_aarch64": "78da94e9a6d652ee6f059770a17f7204508803d7ea8a4d8a1567946e8cfba116",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "a66fe802a04ac5e3506e71ba58b41f5bccdb051d868d8b832f73fea93835092d",
            "linux_aarch64": "0aa566f130d1e26b4e3fea9255fd87e446f978f36243f85b8fa6b7850540fa49",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "b9f9eff3b79a518f05060f9feaf577930adcee1253b85af89c75f543ce979dc6",
            "linux_aarch64": "20dbb79ef39677687ca95c48acd3930f539b38383c20b5651c2914e738ef2f34",
        },
    },
    "v0.7.0": {
        "sudoc": {
            "macos_arm64": "6dd987daace869bbc69e4a08a5ed30dd1cf676d6f0519fabd6f6b31765ae8c9c",
            "linux_x86_64": "afbc58045e6c872ebfbf31b5dd2b412eeb12591d4b38ce663b770e632c41fe26",
            "linux_aarch64": "4e04c11597626aadf300c81af133fa7235c4d61d63b2e056c982d2797c27285c",
        },
        "lockstep_diff": {
            "macos_arm64": "871c9ac870cc8741854c3a191f1021afb48b6e6a7c93f0f77b5f24ba68f933ba",
            "linux_x86_64": "ac412eda6922373eee009900baac023165e45bd1f937e768902bd86b8e4ee58e",
            "linux_aarch64": "b2a0fdea68bda96e6a1b0992e8989a765ea2c8efbea1cb2e8ef24700f9a27ecb",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "a66fe802a04ac5e3506e71ba58b41f5bccdb051d868d8b832f73fea93835092d",
            "linux_aarch64": "0aa566f130d1e26b4e3fea9255fd87e446f978f36243f85b8fa6b7850540fa49",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "b9f9eff3b79a518f05060f9feaf577930adcee1253b85af89c75f543ce979dc6",
            "linux_aarch64": "20dbb79ef39677687ca95c48acd3930f539b38383c20b5651c2914e738ef2f34",
        },
    },
    "v0.6.0": {
        "sudoc": {
            "macos_arm64": "bb91442393e2d2652e905f8ee9aca656c2454edd8290a51de82cd935435636a8",
            "linux_x86_64": "393ffef870e08b68c3542e3fb40b35ad3c6e1095e4731e8282c2b165ae43e697",
            "linux_aarch64": "09d714d725084ef5cad334a1eee8df710d52d08d45461e63d176cd0b61a50c86",
        },
        "lockstep_diff": {
            "macos_arm64": "2f0e73fc758f12f3ee8db71c6290c943f4c2f82783216cf9942e8dc0d9a40dde",
            "linux_x86_64": "bd1b97fbfe378b2a5002a32712ead982e4eca6ca1c2c7692752f3193f7aac174",
            "linux_aarch64": "b63ebaa6f8962ce8962cb664e49aa49ab83f6f8eb5134a552d9b93780342851a",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "d7abf791f5f8477ebf930e39327cae66614eda5f0c4454da8d9c4c95178ee210",
            "linux_aarch64": "9b573b8565e6983c3240942d8e3dc6d9a9538c393c8edc95f9f2490a8b5e12d1",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "76dc1b3c61b64c23b125698b81ee8f258c90c99530222114f75cddede7ba65c0",
            "linux_aarch64": "0ea0dbc90f0b131ccb8219b41a46392bc6079dafd1987a2fdf82dbd916be42b7",
        },
    },
    "v0.5.0": {
        "sudoc": {
            "macos_arm64": "52df4600bfe50f1d3d314ea1ff91120d821a74ef8113f22993dba1636c04fecd",
            "linux_x86_64": "ea827762f834af54a337a28a37a85fdf4574fc3cce3c46898bf98aea5f30e4b4",
            "linux_aarch64": "a35ff8834fe1eaa06616067c9e632b456c412c7fd5b786fd4931520e82566701",
        },
        "lockstep_diff": {
            "macos_arm64": "dc8ba8164bc90c2ae68168e80c0f0c6965fc55ae1e8b3c3a700fdee3b82b5291",
            "linux_x86_64": "0e224af068ed0701cc0e3c8b28d18d2aad013d88918709831987e37a7f1c1423",
            "linux_aarch64": "152adff682e44df1781703f18de158ecd2905bd6ec38f71924371a84c810e83d",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "d7abf791f5f8477ebf930e39327cae66614eda5f0c4454da8d9c4c95178ee210",
            "linux_aarch64": "9b573b8565e6983c3240942d8e3dc6d9a9538c393c8edc95f9f2490a8b5e12d1",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "76dc1b3c61b64c23b125698b81ee8f258c90c99530222114f75cddede7ba65c0",
            "linux_aarch64": "0ea0dbc90f0b131ccb8219b41a46392bc6079dafd1987a2fdf82dbd916be42b7",
        },
    },
    "v0.4.0": {
        "sudoc": {
            "macos_arm64": "bc62ccbf929972b56f12f30fd93e383bdc0314bb3b8f0274bb7f14c76f7ed1a5",
            "linux_x86_64": "b4e547c91759f4c88df64c034b256adb287f582a9764d7122038865125647c00",
            "linux_aarch64": "f9d23d82dac02a785d7df98654198f6bedd72a49b60b0fa98babb59812b1999e",
        },
        "lockstep_diff": {
            "macos_arm64": "dc8ba8164bc90c2ae68168e80c0f0c6965fc55ae1e8b3c3a700fdee3b82b5291",
            "linux_x86_64": "d3e25216ef4556b8d68f18a0776349f76828583665b6cd35dc883d7aae0efd65",
            "linux_aarch64": "7a359bee93a1595438939c93f67a41402af7fdf8d7e0f3b6175f3332e80c92e5",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "d7abf791f5f8477ebf930e39327cae66614eda5f0c4454da8d9c4c95178ee210",
            "linux_aarch64": "9b573b8565e6983c3240942d8e3dc6d9a9538c393c8edc95f9f2490a8b5e12d1",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "76dc1b3c61b64c23b125698b81ee8f258c90c99530222114f75cddede7ba65c0",
            "linux_aarch64": "0ea0dbc90f0b131ccb8219b41a46392bc6079dafd1987a2fdf82dbd916be42b7",
        },
    },
    "v0.3.0": {
        "sudoc": {
            "macos_arm64": "e0bc7110c700bb2a94bc0d680064bb92bf4a2bdea4184b30bafbdc01929ef5ce",
            "linux_x86_64": "fd7e310ad4b7fa797141941725453a0f934a6ec712967425ab5cffcf768d377d",
            "linux_aarch64": "17119337513bb79c0e31a409a6955a529dee9a3ef8561029f52fb5d7895fd388",
        },
        "lockstep_diff": {
            "macos_arm64": "fffbfebd8e4afe60602c6a1b161dbe2e4af4b348a482f14f72f70576dc3abc87",
            "linux_x86_64": "99f366271bce1aa8a5b88bbc79fde22d475e9e0f69767c8bd3a01f2e99b56884",
            "linux_aarch64": "fd7c61b72ef23e8040910478ab74fa3e00e3fd6f11da1272604ff7c3037a8dd7",
        },
        "capture_run": {
            "macos_arm64": "e0a7db5b11b428e2672b9a3f174bcc033bb80a66bc6f13b7d2aaf6ebbb33f4ec",
            "linux_x86_64": "d7abf791f5f8477ebf930e39327cae66614eda5f0c4454da8d9c4c95178ee210",
            "linux_aarch64": "9b573b8565e6983c3240942d8e3dc6d9a9538c393c8edc95f9f2490a8b5e12d1",
        },
        "emit_unpack": {
            "macos_arm64": "a025e5666fe2d663748340892155cf17089169d5bb17618ab6360b5ec5d77f93",
            "linux_x86_64": "76dc1b3c61b64c23b125698b81ee8f258c90c99530222114f75cddede7ba65c0",
            "linux_aarch64": "0ea0dbc90f0b131ccb8219b41a46392bc6079dafd1987a2fdf82dbd916be42b7",
        },
    },
    # v0.2.0 and v0.1.0 predate the matched-set release: they shipped `sudoc`
    # alone, so their entries carry only that asset.
    "v0.2.0": {
        "sudoc": {
            "macos_arm64": "8b0ac472231eb9d8bc5e918578dfdf5b086bb45d509fbc156c5583d1832eec01",
            "linux_x86_64": "de2d0265df272bbf30461fae3d843c9f19f18d4f4a55a8c324c3eb68653932c3",
            "linux_aarch64": "180ac88097db472cb96142a554d05cc3e048b5baa239b3b39e3591d847581aa3",
        },
    },
    "v0.1.0": {
        "sudoc": {
            "macos_arm64": "0829935f9a68a142b6179f58c84508cb9d07c7b08be6253c653677e7a991806b",
            "linux_x86_64": "3343d00da2d6a816671611d0c10b72630e0ad0e5c192975ed47b9ccce5834e94",
            "linux_aarch64": "01906a8354101a6e4cc2b1804e8e6e5862c774ed7813a6332643637d5eb98b07",
        },
    },
}
