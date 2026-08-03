#!/usr/bin/env bash
# Minimal reference external-backend emitter (rules_sudo worked example).
# A thin sh_binary wrapper over emit.py (JSON is clearer in Python than shell) —
# NOT a haskell_binary/py_binary, to show the lowest-common-denominator form a
# third-party plugin author can copy. Reads an emit-request envelope on stdin and
# writes a {files} response on stdout (the unchanged emit protocol).
set -euo pipefail

# Resolve this script's real directory (its runfiles copy) so emit.py — a sibling
# in `data` — is found regardless of how the sh_binary is invoked.
src="${BASH_SOURCE[0]}"
while [ -L "$src" ]; do
  dir="$(cd -P "$(dirname "$src")" && pwd)"
  src="$(readlink "$src")"
  [[ "$src" != /* ]] && src="$dir/$src"
done
here="$(cd -P "$(dirname "$src")" && pwd)"

exec python3 "$here/emit.py"
