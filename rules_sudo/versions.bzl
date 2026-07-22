"""Manifest of pinned sudoc release binaries, by version and platform.

Update when cutting a new sudocode release: add a new top-level version key
with a sha256 for each of the three published platform binaries. Fetch the
`.sha256` assets fresh from the GitHub release (don't trust a stale copy)
and verify them against the downloaded binaries before pinning, e.g.:

    gh release download <tag> --repo hacker6284/sudocode --pattern '*.sha256'
    gh release download <tag> --repo hacker6284/sudocode --pattern 'sudoc-*' --clobber
    shasum -a 256 -c sudoc-<triple>.sha256
"""

# platform_target key -> release asset target-triple suffix. Release assets
# are named `sudoc-<triple>` / `sudoc-<triple>.sha256`.
PLATFORM_TRIPLES = {
    "macos_arm64": "aarch64-apple-darwin",
    "linux_x86_64": "x86_64-unknown-linux-gnu",
    "linux_aarch64": "aarch64-unknown-linux-gnu",
}

# version -> platform_target -> sha256
SUDO_TOOLCHAIN_VERSIONS = {
    "v0.2.0": {
        "macos_arm64": "8b0ac472231eb9d8bc5e918578dfdf5b086bb45d509fbc156c5583d1832eec01",
        "linux_aarch64": "180ac88097db472cb96142a554d05cc3e048b5baa239b3b39e3591d847581aa3",
        "linux_x86_64": "de2d0265df272bbf30461fae3d843c9f19f18d4f4a55a8c324c3eb68653932c3",
    },
    "v0.1.0": {
        "macos_arm64": "0829935f9a68a142b6179f58c84508cb9d07c7b08be6253c653677e7a991806b",
        "linux_x86_64": "3343d00da2d6a816671611d0c10b72630e0ad0e5c192975ed47b9ccce5834e94",
        "linux_aarch64": "01906a8354101a6e4cc2b1804e8e6e5862c774ed7813a6332643637d5eb98b07",
    },
}
