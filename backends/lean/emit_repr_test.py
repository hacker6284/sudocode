"""Pin the Lean emitter's Repr and structure-update rules.

Three asserts, each of which fails if the corresponding emit.py change is
reverted:

1. A wide acyclic record (`Wide`) derives `BEq` only, not `Repr`.
2. A cyclic enum (`Rose`) and the record it carries (`Tag`) derive `BEq, Repr`.
3. A nested field write inside a match arm emits `({ … with … } : T)` with
   the inner record's type, then the outer record's type.

The fixture is backends/lean/repr_rules.sudo. IR comes from `sudoc emit-ir`;
this file checks the Lean `emit.py` prints. No lake build.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import unittest
from pathlib import Path

# Same directory as emit.py, both under Bazel (imports=["."]) and a local run.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import emit  # noqa: E402


def _sudoc_bin() -> Path:
    env = os.environ.get("SUDOC_BIN")
    if env and not env.startswith("$("):
        p = Path(env)
        if not p.is_absolute():
            p = Path.cwd() / p
        if p.is_file():
            return p
    try:
        from python.runfiles import runfiles as runfiles_lib

        r = runfiles_lib.Create()
        if r is not None:
            for key in (
                "sudocode/sudoc/crates/cli/sudoc",
                "_main/sudoc/crates/cli/sudoc",
                "sudoc/crates/cli/sudoc",
            ):
                loc = r.Rlocation(key)
                if loc and os.path.isfile(loc):
                    return Path(loc)
    except ImportError:
        pass
    cand = Path(__file__).resolve().parents[2] / "sudoc" / "target" / "debug" / "sudoc"
    if cand.is_file():
        return cand
    raise FileNotFoundError(
        "emit_repr_test: cannot locate sudoc (SUDOC_BIN or //sudoc/crates/cli:sudoc)"
    )


def _fixture() -> Path:
    sibling = Path(__file__).resolve().parent / "repr_rules.sudo"
    if sibling.is_file():
        return sibling
    candidates: list[str] = []
    try:
        from python.runfiles import runfiles as runfiles_lib

        r = runfiles_lib.Create()
        if r is not None:
            for key in (
                "sudocode/backends/lean/repr_rules.sudo",
                "_main/backends/lean/repr_rules.sudo",
                "backends/lean/repr_rules.sudo",
            ):
                p = r.Rlocation(key)
                if p:
                    candidates.append(p)
    except ImportError:
        pass
    test_srcdir = os.environ.get("TEST_SRCDIR")
    if test_srcdir:
        for root, _dirs, files in os.walk(test_srcdir):
            if "repr_rules.sudo" in files and root.rstrip("/").endswith("lean"):
                candidates.append(os.path.join(root, "repr_rules.sudo"))
                break
    for c in candidates:
        if c and os.path.isfile(c):
            return Path(c)
    raise FileNotFoundError("emit_repr_test: cannot locate repr_rules.sudo")


def _emitted_lean() -> str:
    ir = subprocess.run(
        [str(_sudoc_bin()), "emit-ir", str(_fixture())],
        check=True,
        capture_output=True,
        text=True,
    )
    modules = json.loads(ir.stdout)
    entry = modules[-1]["name"]
    header = json.dumps(
        {"protocol": 4, "cmd": "emit", "entry": entry, "with_tests": False},
        separators=(",", ":"),
    )
    req = emit.decode_request(json.loads(header[:-1] + ',"modules":' + ir.stdout + "}"))
    em = emit.Em(req.modules, req.modules[-1])
    return em.emit_module_src()


def _deriving_after(src: str, decl: str) -> str:
    lines = src.splitlines()
    for i, line in enumerate(lines):
        if line == decl:
            for nxt in lines[i + 1 :]:
                stripped = nxt.strip()
                if stripped.startswith("deriving "):
                    return stripped
                if nxt.startswith(("structure ", "inductive ", "def ", "instance ")):
                    break
    raise AssertionError(f"no deriving clause after {decl!r}")


def _func_body(src: str, name: str) -> str:
    lines = src.splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith(f"def {name} "))
    end = len(lines)
    for j in range(start + 1, len(lines)):
        if lines[j].startswith("def ") or lines[j].startswith("end "):
            end = j
            break
    return "\n".join(lines[start:end])


class LeanReprRulesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.src = _emitted_lean()

    def test_wide_acyclic_record_derives_beq_only(self) -> None:
        clause = _deriving_after(self.src, "structure Wide where")
        self.assertEqual(clause, "deriving BEq")
        self.assertNotIn("Repr", clause)

    def test_cyclic_enum_and_payload_record_derive_repr(self) -> None:
        self.assertEqual(
            _deriving_after(self.src, "inductive Rose : Type where"),
            "deriving BEq, Repr",
        )
        self.assertEqual(
            _deriving_after(self.src, "structure Tag where"),
            "deriving BEq, Repr",
        )

    def test_nested_update_in_match_arm_is_ascribed(self) -> None:
        body = _func_body(self.src, "paint")
        tip, rest = body.split("| .Sudo_4Rose_3Tip =>", 1)
        self.assertIn("match ", tip)
        arm = rest.split("| _ =>", 1)[0]
        inner = re.search(
            r"\(\{ \(w\)\.sudo_4Wide_5inner with sudo_5Inner_1x := \S+ \} : Inner\)",
            arm,
        )
        outer = re.search(
            r"\(\{ w with sudo_4Wide_5inner := \S+ \} : Wide\)",
            arm,
        )
        self.assertIsNotNone(inner, arm)
        self.assertIsNotNone(outer, arm)
        assert inner is not None and outer is not None
        self.assertLess(inner.start(), outer.start())


if __name__ == "__main__":
    unittest.main()
