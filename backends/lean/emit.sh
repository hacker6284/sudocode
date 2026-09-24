#!/usr/bin/env bash
# sudo → Lean 4 emitter, driven as an sh_binary (same shape as backends/haskell).
#
# Speaks protocol 4 (spec/protocol.md §2): one emit-request envelope on stdin,
# one {"files":[...]} or {"error":"..."} response on stdout. Thin wrapper over
# python3 emit.py. emit.py reads SudoRt.lean relative to this directory.
set -euo pipefail

src="${BASH_SOURCE[0]}"
while [ -L "$src" ]; do
  dir="$(cd -P "$(dirname "$src")" && pwd)"
  src="$(readlink "$src")"
  [[ "$src" != /* ]] && src="$dir/$src"
done
cd "$(cd -P "$(dirname "$src")" && pwd)"

export PYTHONIOENCODING=utf-8
exec python3 emit.py "$@"
