#!/usr/bin/env python3
"""Every shipped tag must have a literal entry in rules_sudo/versions.bzl.

Why this exists. The release workflow injects real sha256s into the
INJECT-RELEASE block of the versions.bzl that goes INTO the release tarball,
so a consumer who downloads the tarball gets a self-pinning ruleset. Nothing
writes those values back into the repo. Promoting the just-released block to a
literal entry -- so a consumer building rules_sudo FROM SOURCE can pin that
version -- is a manual step, and it was silently skipped for v0.5.0: the repo
went straight from _PENDING_VERSION = "v0.5.0" to the next tag, and v0.5.0 was
simply absent from the table until backfilled by hand on 2026-08-14 from the
published .sha256 assets.

`inject_release_shas_test` covers the injection SCRIPT. It cannot catch a
missing promotion, because the promotion is not the script's job. This test is
the forcing function: if a git tag exists with no corresponding literal entry,
the build fails and names the tag.

Deliberately tolerant in two ways, so it fails only on the real defect:
  - Tags before the first literal entry are ignored. The table does not claim
    to reach back to the beginning of the project.
  - _PENDING_VERSION is exempt. It names the NEXT planned tag, which by
    definition has not shipped and has no shas yet.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import unittest


def _repo_root() -> str:
    # Under `bazel test`, the runfiles cwd is the workspace root.
    return os.environ.get("BUILD_WORKSPACE_DIRECTORY", os.getcwd())


def _versions_bzl(root: str) -> str:
    with open(os.path.join(root, "rules_sudo", "versions.bzl")) as f:
        return f.read()


def _literal_versions(src: str) -> set[str]:
    """Tags with a literal `"vX.Y.Z": {` entry in SUDO_TOOLCHAIN_VERSIONS."""
    return set(re.findall(r'^\s{4}"(v\d+\.\d+\.\d+)":\s*\{', src, re.M))


def _pending_version(src: str) -> str | None:
    m = re.search(r'^_PENDING_VERSION\s*=\s*"([^"]+)"', src, re.M)
    return m.group(1) if m else None


def _git_tags(root: str) -> list[str]:
    try:
        out = subprocess.run(
            ["git", "-C", root, "tag", "--list", "v*"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []
    return [t for t in out.split() if re.fullmatch(r"v\d+\.\d+\.\d+", t)]


def _key(tag: str) -> tuple[int, ...]:
    return tuple(int(p) for p in tag.lstrip("v").split("."))


class VersionsTableComplete(unittest.TestCase):
    def test_every_shipped_tag_has_a_literal_entry(self) -> None:
        root = _repo_root()
        tags = _git_tags(root)
        if not tags:
            self.skipTest("no git tags visible (shallow clone or no .git)")

        src = _versions_bzl(root)
        literals = _literal_versions(src)
        self.assertTrue(literals, "no literal version entries found at all")

        pending = _pending_version(src)
        oldest = min(literals, key=_key)

        missing = [
            t for t in tags
            if _key(t) > _key(oldest) and t not in literals and t != pending
        ]

        self.assertFalse(
            missing,
            "shipped tag(s) with no literal entry in rules_sudo/versions.bzl: "
            + ", ".join(sorted(missing, key=_key))
            + ".\nThe release workflow injects shas into the TARBALL's copy, not "
            "the repo's. After a release, promote the INJECT-RELEASE block to a "
            "literal entry using the published <asset>-<triple>.sha256 files, "
            "then point _PENDING_VERSION at the next planned tag. Without that, "
            "consumers building rules_sudo from source cannot pin the version.",
        )


if __name__ == "__main__":
    unittest.main()
