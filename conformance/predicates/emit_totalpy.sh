#!/usr/bin/env bash
# totalpy emitter. The request is already gated by `emit-ir --require`.
# This script does not implement a predicate; it re-emits with the Python backend.
set -euo pipefail

# Bazel invokes the symlink that sits next to `<name>.runfiles`. Following
# that symlink first lands in the source tree, where `sudoc` is not a binary.
# stdout is the emit response, so every miss stays on stderr.
rel="sudoc/crates/cli/sudoc"
argv0="${BASH_SOURCE[0]}"
if [[ "$argv0" != /* ]]; then
  argv0="$(pwd)/$argv0"
fi

rf=""
if [[ -d "${argv0}.runfiles" ]]; then
  rf="${argv0}.runfiles"
elif [[ -n "${RUNFILES_DIR:-}" && -d "${RUNFILES_DIR}" ]]; then
  rf="${RUNFILES_DIR}"
fi

sudoc=""
if [[ -n "$rf" ]]; then
  ws=""
  for ws in _main sudocode ${TEST_WORKSPACE:-}; do
    cand="${rf}/${ws}/${rel}"
    if [[ -x "$cand" ]]; then
      sudoc="$cand"
      break
    fi
  done
  if [[ -z "$sudoc" && -x "${rf}/${rel}" ]]; then
    sudoc="${rf}/${rel}"
  fi
fi

if [[ -z "$sudoc" ]]; then
  manifest="${RUNFILES_MANIFEST_FILE:-${argv0}.runfiles_manifest}"
  if [[ -f "$manifest" ]]; then
    line="$(grep -m1 -E '(^|/)sudoc/crates/cli/sudoc( |$)' "$manifest" || true)"
    if [[ -n "$line" ]]; then
      path="${line#* }"
      if [[ -x "$path" ]]; then
        sudoc="$path"
      fi
    fi
  fi
fi

if [[ -z "$sudoc" ]]; then
  echo "emit_totalpy: cannot locate sudoc in runfiles" >&2
  exit 1
fi

exec "$sudoc" emit-inprocess --target py
