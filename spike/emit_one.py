#!/usr/bin/env python3
"""Emit one cryptoys .sudo through the Lean protocol-4 backend.

Reproduces the Bazel codegen leaf from rules_sudo/private/lockstep.bzl:
  sudoc emit-ir [-I stdlib] [--require terminates] FILE
  → wrap {protocol:4, cmd:emit, entry, with_tests:true, modules}
  → python3 backends/lean/emit.py
  → unpack files/

This is a spike helper, not a new registration path.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SUDOC = ROOT / "sudoc" / "target" / "release" / "sudoc"
EMIT_PY = ROOT / "backends" / "lean" / "emit.py"
STDLIB = ROOT / "stdlib"


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print("+", " ".join(cmd), flush=True)
    return subprocess.run(cmd, **kw)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("sudo_file")
    ap.add_argument("--out", required=True, help="output directory")
    ap.add_argument(
        "--require",
        action="append",
        default=[],
        help="predicate name (repeatable). empty = full peer, as Lean is registered",
    )
    ap.add_argument("--no-tests", action="store_true", help="set with_tests=false")
    args = ap.parse_args()

    src = Path(args.sudo_file).resolve()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    stem = src.stem

    ir_path = out / "modules.json"
    req_path = out / "request.json"
    resp_path = out / "response.json"
    files_dir = out / "files"
    files_dir.mkdir(exist_ok=True)

    cmd = [str(SUDOC), "emit-ir", "-I", str(STDLIB)]
    for pred in args.require:
        cmd.extend(["--require", pred])
    cmd.extend(["-o", str(ir_path), str(src)])
    r = run(cmd)
    if r.returncode != 0:
        print(f"emit-ir failed rc={r.returncode}", file=sys.stderr)
        return r.returncode

    modules_text = ir_path.read_text()
    # Pure concatenation, same as lockstep.bzl (modules.json is a JSON array).
    header = json.dumps(
        {
            "protocol": 4,
            "cmd": "emit",
            "entry": stem,
            "with_tests": not args.no_tests,
        },
        separators=(",", ":"),
    )
    # Replace the closing } with ,"modules": <array>}
    envelope = header[:-1] + ',"modules":' + modules_text + "}"
    req_path.write_text(envelope)

    # Validate envelope parses (catches concat bugs before emit.py).
    json.loads(envelope)

    r = run(
        [sys.executable, str(EMIT_PY)],
        cwd=str(EMIT_PY.parent),
        input=envelope.encode(),
        stdout=open(resp_path, "wb"),
    )
    if r.returncode != 0:
        print(f"emit.py failed rc={r.returncode}", file=sys.stderr)
        return r.returncode

    resp = json.loads(resp_path.read_text())
    if "error" in resp:
        print("EMITTER ERROR:", resp["error"], file=sys.stderr)
        (out / "EMIT_ERROR.txt").write_text(resp["error"] + "\n")
        return 2
    if "files" not in resp:
        print("malformed emit response keys:", list(resp), file=sys.stderr)
        return 2

    manifest = []
    for item in resp["files"]:
        rel = item["path"]
        dest = files_dir / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(item["contents"])
        manifest.append({"path": rel, "bytes": len(item["contents"])})
        print(f"  wrote {dest} ({len(item['contents'])} bytes)")
    (out / "files_manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"OK {stem} → {files_dir} ({len(manifest)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
