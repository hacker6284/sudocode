#!/usr/bin/env bash
# totalpy emitter. The request is already gated by `emit-ir --require`.
# This script does not implement a predicate.
set -euo pipefail

# Do not readlink. Bazel runs the symlink beside `<name>.runfiles`; the
# realpath is the source tree, which has no sudoc binary.
argv0="${BASH_SOURCE[0]}"
if [[ "$argv0" != /* ]]; then
  argv0="$(pwd)/$argv0"
fi
sudoc="${argv0}.runfiles/_main/sudoc/crates/cli/sudoc"
if [[ ! -x "$sudoc" ]]; then
  sudoc="${argv0}.runfiles/sudoc/crates/cli/sudoc"
fi
if [[ ! -x "$sudoc" ]]; then
  echo "emit_totalpy: cannot locate sudoc in runfiles" >&2
  exit 1
fi
exec "$sudoc" emit-inprocess --target py
