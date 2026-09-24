#!/usr/bin/env bash
# sudo → Lean 4 emitter (protocol 4).
#
# Speaks the emit protocol (spec/protocol.md §2): one emit-request envelope on
# stdin, one {"files":[...]} response on stdout. Thin wrapper over emit.py —
# Python is already on every CI image, so codegen does not need Lean on PATH.
# Recipe compile/run (lake / lean) happens later, in the lockstep run-leaf.
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
