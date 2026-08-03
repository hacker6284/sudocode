#!/usr/bin/env bash
# Build the parent repo's HEAD sudoc/lockstep_diff/capture_run/emit_unpack, point
# this example's MODULE.bazel sudo.local_binary at them, and run the reference
# backend's lockstep test. This is the pre-release dogfood mechanism (design §8
# Phase 5 Task 8): validate an external backend against HEAD binaries before any
# release exists. After the matched-pair release, use sudo.toolchain instead.
set -euo pipefail

here="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"  # rules_sudo/examples/reference_backend -> repo root

echo "==> Building HEAD binaries in $repo"
(cd "$repo" && bazel build \
  //sudoc/crates/cli:sudoc \
  //sudoc/crates/harness:lockstep_diff \
  //sudoc/crates/harness:capture_run \
  //sudoc/crates/harness:emit_unpack)

sudoc="$(cd "$repo" && realpath "$(bazel cquery --output=files //sudoc/crates/cli:sudoc 2>/dev/null)")"
diff="$(cd "$repo" && realpath "$(bazel cquery --output=files //sudoc/crates/harness:lockstep_diff 2>/dev/null)")"
cap="$(cd "$repo" && realpath "$(bazel cquery --output=files //sudoc/crates/harness:capture_run 2>/dev/null)")"
unp="$(cd "$repo" && realpath "$(bazel cquery --output=files //sudoc/crates/harness:emit_unpack 2>/dev/null)")"

echo "==> Wiring MODULE.bazel local_binary at HEAD binaries"
python3 - "$here/MODULE.bazel" "$sudoc" "$diff" "$cap" "$unp" <<'PY'
import re, sys
path, sudoc, diff, cap, unp = sys.argv[1:6]
s = open(path).read()
s = re.sub(r'sudoc = "[^"]*"', 'sudoc = "%s"' % sudoc, s, count=1)
s = re.sub(r'lockstep_diff = "[^"]*"', 'lockstep_diff = "%s"' % diff, s, count=1)
s = re.sub(r'capture_run = "[^"]*"', 'capture_run = "%s"' % cap, s, count=1)
s = re.sub(r'emit_unpack = "[^"]*"', 'emit_unpack = "%s"' % unp, s, count=1)
open(path, "w").write(s)
PY

echo "==> Running the reference-backend lockstep test"
(cd "$here" && bazel test //:hello_lockstep_test --test_output=errors)
echo "==> PASS: the :pyref external backend lockstep-agrees with the built-in py backend."
