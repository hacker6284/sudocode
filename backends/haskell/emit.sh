#!/usr/bin/env bash
# sudo -> Haskell emitter, driven as an sh_binary (Bazel migration Phase 5).
#
# Speaks the UNCHANGED emit protocol (spec/protocol.md §2): reads one emit
# request envelope on stdin, writes one {"files":[...]} response on stdout. This
# is a thin wrapper over `runghc Emit.hs` — NOT a haskell_binary (rules_haskell
# stays out of scope). Emit.hs imports SudoJson.hs and `readFile "SudoRt.hs"`
# relative to cwd, so we cd to the directory holding the .hs sources first.
set -euo pipefail

# Resolve this script's real location (its runfiles/source dir), following
# symlinks, so Emit.hs / SudoJson.hs / SudoRt.hs (its siblings) are found.
src="${BASH_SOURCE[0]}"
while [ -L "$src" ]; do
  dir="$(cd -P "$(dirname "$src")" && pwd)"
  src="$(readlink "$src")"
  [[ "$src" != /* ]] && src="$dir/$src"
done
cd "$(cd -P "$(dirname "$src")" && pwd)"

# GHC decodes source/`readFile` bytes under the locale encoding; Bazel actions
# run with a sanitized env (no LANG), so GHC defaults to ASCII and chokes on the
# runtime's UTF-8 bytes. Force a UTF-8 locale, portably: glibc (Linux CI) always
# has C.UTF-8; macOS lacks it but has en_US.UTF-8.
if locale -a 2>/dev/null | grep -qiE '^C\.utf-?8$'; then
  export LC_ALL=C.UTF-8
else
  export LC_ALL=en_US.UTF-8
fi
export LANG="$LC_ALL"

exec runghc Emit.hs "$@"
