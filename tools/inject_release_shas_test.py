"""Unit tests for tools/inject_release_shas.py.

Covers: (1) PLATFORM_TRIPLES / RELEASE_ASSETS drift vs rules_sudo/versions.bzl,
(2) inject round-trip + re-inject replace, (3) fail-loudly exit codes for
missing sha, malformed sha, and bad tag. No network; stdlib unittest only.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import inject_release_shas


def _versions_bzl_path() -> Path:
    """Locate rules_sudo/versions.bzl via Bazel runfiles (data dep)."""
    candidates = []
    try:
        from python.runfiles import runfiles as runfiles_lib

        r = runfiles_lib.Create()
        if r is not None:
            for key in (
                "sudocode/rules_sudo/versions.bzl",
                "_main/rules_sudo/versions.bzl",
                "rules_sudo/versions.bzl",
            ):
                p = r.Rlocation(key)
                if p:
                    candidates.append(p)
    except ImportError:
        pass

    # Fallback: walk TEST_SRCDIR / adjacent to this file (local non-Bazel runs).
    test_srcdir = os.environ.get("TEST_SRCDIR")
    if test_srcdir:
        for root, _dirs, files in os.walk(test_srcdir):
            if "versions.bzl" in files and root.rstrip("/").endswith("rules_sudo"):
                candidates.append(os.path.join(root, "versions.bzl"))
                break
    here = Path(__file__).resolve()
    for parent in [here.parent, *here.parents]:
        cand = parent / "rules_sudo" / "versions.bzl"
        if cand.is_file():
            candidates.append(str(cand))
            break

    for c in candidates:
        if c and os.path.isfile(c):
            return Path(c)
    raise FileNotFoundError(
        "inject_release_shas_test: cannot locate rules_sudo/versions.bzl in "
        "runfiles (data = [//rules_sudo:versions.bzl])"
    )


def _parse_platform_triples(src: str) -> list[tuple[str, str]]:
    m = re.search(r"PLATFORM_TRIPLES\s*=\s*\{([^}]*)\}", src, re.DOTALL)
    if not m:
        raise AssertionError("PLATFORM_TRIPLES not found in versions.bzl")
    return re.findall(r'"([^"]+)"\s*:\s*"([^"]+)"', m.group(1))


def _parse_release_assets(src: str) -> list[str]:
    m = re.search(r"RELEASE_ASSETS\s*=\s*\[([^\]]*)\]", src, re.DOTALL)
    if not m:
        raise AssertionError("RELEASE_ASSETS not found in versions.bzl")
    return re.findall(r'"([^"]+)"', m.group(1))


def _fake_sha(n: int) -> str:
    """Deterministic well-formed 64-hex sha distinct per n."""
    return ("%064x" % (n + 1))[-64:]


def _write_sha_files(sha_dir: Path, omit: tuple[str, str] | None = None,
                     corrupt: tuple[str, str, str] | None = None) -> dict[str, str]:
    """Write 12 asset×triple .sha256 files. Returns map filename→sha.

    omit: (asset, triple) to skip writing.
    corrupt: (asset, triple, bad_content) to write a non-64-hex payload.
    """
    shas = {}
    i = 0
    for asset in inject_release_shas.RELEASE_ASSETS:
        for _plat, triple in inject_release_shas.PLATFORM_TRIPLES:
            i += 1
            name = "%s-%s.sha256" % (asset, triple)
            if omit and omit == (asset, triple):
                continue
            if corrupt and corrupt[0] == asset and corrupt[1] == triple:
                content = corrupt[2]
                (sha_dir / name).write_text(content + "  " + name + "\n")
                shas[name] = content
                continue
            sha = _fake_sha(i * 0x1111)
            (sha_dir / name).write_text("%s  %s-%s\n" % (sha, asset, triple))
            shas[name] = sha
    return shas


def _fixture_versions(path: Path) -> None:
    path.write_text(
        "# preamble\n"
        "SOME_OTHER = 1\n"
        "# INJECT-RELEASE-BEGIN\n"
        '_PENDING_VERSION = "v0.0.0"\n'
        "_PENDING = {\n"
        '    "sudoc": {"macos_arm64": "", "linux_x86_64": "", "linux_aarch64": ""},\n'
        "}\n"
        "# INJECT-RELEASE-END\n"
        "# postamble keeps surrounding text\n"
        'TAIL = "ok"\n'
    )


def _run_inject(versions: Path, sha_dir: Path, tag: str) -> subprocess.CompletedProcess:
    script = Path(inject_release_shas.__file__).resolve()
    return subprocess.run(
        [sys.executable, str(script), str(versions), str(sha_dir), tag],
        capture_output=True,
        text=True,
    )


class DriftGuardTest(unittest.TestCase):
    def test_platform_triples_and_assets_match_versions_bzl(self):
        src = _versions_bzl_path().read_text()
        bzl_triples = _parse_platform_triples(src)
        bzl_assets = _parse_release_assets(src)
        self.assertEqual(
            list(inject_release_shas.PLATFORM_TRIPLES),
            bzl_triples,
            "PLATFORM_TRIPLES drifted between inject_release_shas.py and versions.bzl",
        )
        self.assertEqual(
            list(inject_release_shas.RELEASE_ASSETS),
            bzl_assets,
            "RELEASE_ASSETS drifted between inject_release_shas.py and versions.bzl",
        )


class RoundTripTest(unittest.TestCase):
    def test_inject_and_reinject(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            versions = tmp_path / "versions.bzl"
            sha_dir = tmp_path / "shas"
            sha_dir.mkdir()
            _fixture_versions(versions)
            shas = _write_sha_files(sha_dir)

            proc = _run_inject(versions, sha_dir, "v0.4.0")
            self.assertEqual(proc.returncode, 0, proc.stderr or proc.stdout)
            out = versions.read_text()
            self.assertIn('_PENDING_VERSION = "v0.4.0"', out)
            self.assertIn("# INJECT-RELEASE-BEGIN", out)
            self.assertIn("# INJECT-RELEASE-END", out)
            self.assertIn('TAIL = "ok"', out)
            self.assertIn("SOME_OTHER = 1", out)
            for sha in shas.values():
                self.assertIn(sha, out)

            # Re-inject with a different tag against the already-injected file.
            proc2 = _run_inject(versions, sha_dir, "v9.9.9")
            self.assertEqual(proc2.returncode, 0, proc2.stderr or proc2.stdout)
            out2 = versions.read_text()
            self.assertIn('_PENDING_VERSION = "v9.9.9"', out2)
            self.assertNotIn('_PENDING_VERSION = "v0.4.0"', out2)
            # Single inject block (not appended).
            self.assertEqual(out2.count("# INJECT-RELEASE-BEGIN"), 1)
            self.assertEqual(out2.count("# INJECT-RELEASE-END"), 1)
            self.assertEqual(out2.count("_PENDING_VERSION"), 1)
            for sha in shas.values():
                self.assertIn(sha, out2)


class FailureModesTest(unittest.TestCase):
    def test_missing_sha_file_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            versions = tmp_path / "versions.bzl"
            sha_dir = tmp_path / "shas"
            sha_dir.mkdir()
            _fixture_versions(versions)
            _write_sha_files(
                sha_dir,
                omit=("sudoc", "x86_64-unknown-linux-gnu"),
            )
            proc = _run_inject(versions, sha_dir, "v0.4.0")
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("missing sha file", proc.stderr + proc.stdout)

    def test_malformed_sha_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            versions = tmp_path / "versions.bzl"
            sha_dir = tmp_path / "shas"
            sha_dir.mkdir()
            _fixture_versions(versions)
            _write_sha_files(
                sha_dir,
                corrupt=("lockstep_diff", "aarch64-apple-darwin", "not_a_real_sha"),
            )
            proc = _run_inject(versions, sha_dir, "v0.4.0")
            self.assertNotEqual(proc.returncode, 0)
            combined = proc.stderr + proc.stdout
            self.assertIn("does not hold a 64-hex sha256", combined)

    def test_bad_tag_exits_nonzero_before_sha_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            versions = tmp_path / "versions.bzl"
            sha_dir = tmp_path / "shas"
            sha_dir.mkdir()
            # No sha files at all — bad tag must fail before missing-sha checks.
            _fixture_versions(versions)
            for bad in ("0.4.0", "v1.2", "v1.2.3-rc1", "vv1.0.0"):
                proc = _run_inject(versions, sha_dir, bad)
                self.assertNotEqual(proc.returncode, 0, bad)
                combined = proc.stderr + proc.stdout
                self.assertIn("does not match", combined, bad)
                self.assertNotIn("missing sha file", combined, bad)


if __name__ == "__main__":
    unittest.main()
