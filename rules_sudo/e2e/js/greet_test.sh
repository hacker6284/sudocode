#!/usr/bin/env bash
# Consumes a sudo_js_library tree artifact: $1 is the directory path
# (from $(rootpath //sudo:greet_js)). Entry module basename is known from
# entry = "greet.sudo" → greet.mjs; we never glob the tree.
set -euo pipefail

DIR="${1:?usage: greet_test.sh <tree-artifact-dir>}"

# Resolve relative paths against the test's working directory (execroot).
if [[ ! -d "$DIR" ]]; then
  # Fallback: runfiles layout when $(rootpath) is not cwd-relative.
  if [[ -n "${TEST_SRCDIR:-}" ]]; then
    WS="${TEST_WORKSPACE:-_main}"
    if [[ -d "${TEST_SRCDIR}/${WS}/${DIR}" ]]; then
      DIR="${TEST_SRCDIR}/${WS}/${DIR}"
    elif [[ -d "${TEST_SRCDIR}/${DIR}" ]]; then
      DIR="${TEST_SRCDIR}/${DIR}"
    fi
  fi
fi

if [[ ! -d "$DIR" ]]; then
  echo "FAIL: tree artifact directory not found: $DIR" >&2
  exit 1
fi

if [[ ! -f "$DIR/greet.mjs" ]]; then
  echo "FAIL: expected $DIR/greet.mjs" >&2
  ls -la "$DIR" >&2 || true
  exit 1
fi

# Prefer absolute file URL for Node ESM when DIR is absolute; otherwise
# relative import from cwd.
if [[ "$DIR" = /* ]]; then
  SPEC="file://${DIR}/greet.mjs"
else
  SPEC="./${DIR}/greet.mjs"
fi

node -e "
import('${SPEC}').then(m => {
  const r = m.greet('hi');
  if (r !== 'HI !') { console.error('FAIL: got', r); process.exit(1); }
  console.log('PASSED');
}).catch(e => { console.error(e); process.exit(1); });
"
