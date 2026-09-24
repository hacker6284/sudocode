#!/usr/bin/env bash
# Install / pin elan + Lean 4.14.0 (lake ships with that toolchain).
#
# CI calls this AFTER `bazel build`, like setup-swift: Lean codegen is
# Python-only; `lake` is a host run-leaf at test time. See
# backends/lean/README.md.
set -euo pipefail

ELAN_HOME="${ELAN_HOME:-$HOME/.elan}"
TOOLCHAIN="leanprover/lean4:v4.14.0"

if [ ! -x "$ELAN_HOME/bin/elan" ]; then
  curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf \
    | sh -s -- -y --default-toolchain "$TOOLCHAIN"
fi

export PATH="$ELAN_HOME/bin:$PATH"
elan toolchain install "$TOOLCHAIN"
elan default "$TOOLCHAIN"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$ELAN_HOME/bin" >> "$GITHUB_PATH"
fi

# Host Lean env for Bazel local-test inheritance (lockstep.bzl
# RunEnvironmentInfo). LEAN_SYSROOT is not a DYLD_* var, so SIP-protected
# launchers can pass it; unsigned capture_run then applies loader paths.
if command -v lean >/dev/null 2>&1; then
  _lean_prefix="$(lean --print-prefix 2>/dev/null || true)"
  if [ -n "${_lean_prefix}" ]; then
    export LEAN_SYSROOT="${_lean_prefix}"
    export LEAN_PATH="${LEAN_PATH:-${_lean_prefix}/lib/lean}"
  fi
fi
export ELAN_HOME="$ELAN_HOME"

if [ -n "${GITHUB_ENV:-}" ]; then
  echo "ELAN_HOME=$ELAN_HOME" >> "$GITHUB_ENV"
  if [ -n "${LEAN_SYSROOT:-}" ]; then
    echo "LEAN_SYSROOT=$LEAN_SYSROOT" >> "$GITHUB_ENV"
    echo "LEAN_PATH=$LEAN_PATH" >> "$GITHUB_ENV"
  fi
fi

lean --version
lake --version
if [ -n "${LEAN_SYSROOT:-}" ]; then
  echo "LEAN_SYSROOT=$LEAN_SYSROOT"
fi
