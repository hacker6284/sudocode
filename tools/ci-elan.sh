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

lean --version
lake --version
