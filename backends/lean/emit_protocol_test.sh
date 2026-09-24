#!/usr/bin/env bash
# Protocol-4 emitter checks that do not need Lean or sudoc.
set -euo pipefail

resolve_root() {
  local cand
  for cand in \
    "${RUNFILES_DIR:-}/_main/backends/lean" \
    "${RUNFILES_DIR:-}/sudocode/backends/lean" \
    "${TEST_SRCDIR:-}/_main/backends/lean" \
    "${TEST_SRCDIR:-}/sudocode/backends/lean" \
    "$(cd "$(dirname "$0")" && pwd)"
  do
    if [[ -n "$cand" && -f "$cand/emit.py" ]]; then
      printf '%s' "$cand"
      return 0
    fi
  done
  return 1
}

ROOT="$(resolve_root)"
EMIT="$ROOT/emit.py"
DATA="$ROOT/testdata"

check() {
  local req="$1"
  local py="$2"
  python3 "$EMIT" < "$DATA/$req" | python3 -c "$py"
}

check sum_request.json '
import json, sys
resp = json.load(sys.stdin)
assert "error" not in resp, resp
paths = {f["path"] for f in resp["files"]}
assert "SudoRt.lean" in paths, paths
assert "sum_test.lean" in paths, paths
src = next(f["contents"] for f in resp["files"] if f["path"] == "sum_test.lean")
assert "def sum_to" in src, src[:500]
assert "SudoRt.forRange" in src
assert "def test_sums" in src
assert "def main" in src
print("sum_request: ok")
'

check while_request.json '
import json, sys
resp = json.load(sys.stdin)
assert "error" in resp, resp
assert "while" in resp["error"].lower(), resp
print("while_request: ok")
'

check bad_protocol.json '
import json, sys
resp = json.load(sys.stdin)
assert "error" in resp, resp
assert "PROTOCOL MISMATCH" in resp["error"], resp
print("bad_protocol: ok")
'
