#!/usr/bin/env bash
# Shared BuildBuddy remote-cache + BES setup for CI Bazel invocations, deduped
# from the three ci.yml steps (Phase 5 Task 6). Usage:
#
#   BB_KEY=<secret> tools/ci-bazel.sh <build|test> TARGET...
#
# Reads BB_KEY (the BUILDBUDDY_API_KEY secret) and GITHUB_EVENT_NAME (a built-in
# GitHub Actions env var). Cache WRITE (--remote_upload_local_results) is gated
# to trusted pushes; PRs — including forks — are read-only. If BB_KEY is unset on
# a push, warns and runs cache-less (so the cache + its fork-PR write-gating are
# never silently assumed). `test` adds --test_output=errors.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: tools/ci-bazel.sh <build|test> TARGET..." >&2
  exit 2
fi
cmd="$1"
shift

FLAGS=(--config=clippy)
if [ "$cmd" = "test" ]; then
  FLAGS+=(--test_output=errors)
fi

if [ -z "${BB_KEY:-}" ] && [ "${GITHUB_EVENT_NAME:-}" = "push" ]; then
  echo "::warning::BUILDBUDDY_API_KEY is not set — the BuildBuddy remote cache + BES are DISABLED on this trusted push, so the cache and its fork-PR write-gating run untested. Set the BUILDBUDDY_API_KEY repository secret to enable them."
fi

if [ -n "${BB_KEY:-}" ]; then
  FLAGS+=(--remote_cache=grpcs://remote.buildbuddy.io)
  FLAGS+=(--remote_header=x-buildbuddy-api-key="$BB_KEY")
  FLAGS+=(--bes_backend=grpcs://remote.buildbuddy.io)
  FLAGS+=(--bes_results_url=https://app.buildbuddy.io/invocation/)
  if [ "${GITHUB_EVENT_NAME:-}" = "push" ]; then
    FLAGS+=(--remote_upload_local_results)
  else
    FLAGS+=(--noremote_upload_local_results)
  fi
fi

exec bazel "$cmd" "${FLAGS[@]}" "$@"
