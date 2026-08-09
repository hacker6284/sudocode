#!/usr/bin/env bash
# bench/run.sh — wall-clock harness for sudo List ops across backends.
#
# Times sudoc-codegen'd artifacts directly (not via `bazel test`). Default
# backends are py + hs; other in-tree targets are supported via --backends.
# See bench/README.md for usage and baseline numbers.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUDOC="${ROOT}/bazel-bin/sudoc/crates/cli/sudoc"
EMIT_UNPACK="${ROOT}/bazel-bin/sudoc/crates/harness/emit_unpack"
HS_EMIT="${ROOT}/backends/haskell/emit.sh"

# In-tree targets that `sudoc build --target` accepts.
INTREE_BACKENDS=(py js c rs zig swift)
# External backends driven by the protocol-3 emit path.
ALL_BACKENDS=(py js c rs zig swift hs)

DEFAULT_BACKENDS=(py hs)
DEFAULT_PROGRAMS=(list_append list_indexed)

SIZE_OVERRIDE=""
TIMEOUT_SECS=""
BACKENDS=()
PROGRAMS=()
WORK_ROOT=""
ANY_FAIL=0
TIMEOUT_WARNED=0

# Populated by run_step / parse_tap / read_knobs / prepare_source.
_RUN_OUT=""
_RUN_RC=0
_RUN_TIMED_OUT=0
_RUN_S="0.00"
_TAP_OK=0
_N="?"
_M=""
_SRC=""

usage() {
  cat <<'EOF'
Usage: bench/run.sh [options] [program|backend ...]

Options:
  --backends LIST   Comma-separated backends (default: py,hs).
                    In-tree: py,js,c,rs,zig,swift. External: hs.
  --size N          Override module-level `N = ...` before building
                    (temp copy only; does not edit the committed .sudo).
  --timeout SECS    Bound only the run step (not codegen/build). On
                    timeout the row status is `timeout` and the script
                    continues; overall exit is still 0 unless a
                    non-timeout correctness failure occurs.
  -h, --help        Show this help.

Positionals:
  program names (file stems under bench/, e.g. list_append) and/or
  backend names. Defaults: both list_append and list_indexed on py,hs.

Examples:
  bench/run.sh
  bench/run.sh --backends py,hs list_append
  bench/run.sh --size 1000000 --timeout 120 list_append hs
EOF
}

is_backend() {
  local b="$1" x
  for x in "${ALL_BACKENDS[@]}"; do
    [[ "$x" == "$b" ]] && return 0
  done
  return 1
}

is_intree() {
  local b="$1" x
  for x in "${INTREE_BACKENDS[@]}"; do
    [[ "$x" == "$b" ]] && return 0
  done
  return 1
}

now_s() {
  python3 -c 'import time; print(f"{time.time():.6f}")'
}

elapsed_s() {
  python3 -c 'import sys; print(f"{float(sys.argv[2]) - float(sys.argv[1]):.2f}")' "$1" "$2"
}

sum_s() {
  python3 -c 'import sys; print(f"{float(sys.argv[1])+float(sys.argv[2]):.2f}")' "$1" "$2"
}

ensure_bins() {
  if [[ ! -x "$SUDOC" || ! -x "$EMIT_UNPACK" ]]; then
    echo "building sudoc + emit_unpack via bazel..." >&2
    (cd "$ROOT" && bazel build //sudoc/crates/cli:sudoc //sudoc/crates/harness:emit_unpack)
  fi
  if [[ ! -x "$SUDOC" ]]; then
    echo "error: missing $SUDOC after bazel build" >&2
    exit 2
  fi
  if [[ ! -x "$EMIT_UNPACK" ]]; then
    echo "error: missing $EMIT_UNPACK after bazel build" >&2
    exit 2
  fi
  if [[ ! -x "$HS_EMIT" ]]; then
    echo "error: missing $HS_EMIT" >&2
    exit 2
  fi
}

find_timeout_bin() {
  if command -v timeout >/dev/null 2>&1; then
    command -v timeout
  elif command -v gtimeout >/dev/null 2>&1; then
    command -v gtimeout
  else
    echo ""
  fi
}

# Run argv under an optional wall-clock timeout. Sets:
#   _RUN_RC, _RUN_OUT, _RUN_TIMED_OUT (0|1), _RUN_S
run_step() {
  local timeout_secs="$1"
  shift
  local start end out_file rc timed_out=0
  out_file="$(mktemp "${WORK_ROOT}/run_out.XXXXXX")"

  start="$(now_s)"
  if [[ -n "$timeout_secs" && "$timeout_secs" -gt 0 ]]; then
    local tbin
    tbin="$(find_timeout_bin)"
    if [[ -n "$tbin" ]]; then
      set +e
      # GNU timeout: --kill-after is portable on coreutils; ignore if unsupported.
      if "$tbin" --help 2>&1 | grep -q -- '--kill-after'; then
        "$tbin" --signal=TERM --kill-after=2s "$timeout_secs" "$@" >"$out_file" 2>&1
      else
        "$tbin" "$timeout_secs" "$@" >"$out_file" 2>&1
      fi
      rc=$?
      set -e
      # GNU coreutils timeout → 124; SIGKILL → 137.
      if [[ $rc -eq 124 || $rc -eq 137 ]]; then
        timed_out=1
      fi
    else
      if [[ "$TIMEOUT_WARNED" -eq 0 ]]; then
        echo "warning: neither timeout nor gtimeout on PATH; using bash watchdog for --timeout" >&2
        TIMEOUT_WARNED=1
      fi
      # Disable monitor mode so a killed background job does not print
      # "Terminated: 15" noise to the terminal.
      set +e
      set +m
      "$@" >"$out_file" 2>&1 &
      local pid=$!
      (
        sleep "$timeout_secs"
        if kill -0 "$pid" 2>/dev/null; then
          kill -TERM "$pid" 2>/dev/null || true
          sleep 1
          kill -KILL "$pid" 2>/dev/null || true
        fi
      ) &
      local watchdog=$!
      wait "$pid" 2>/dev/null
      rc=$?
      kill "$watchdog" 2>/dev/null || true
      wait "$watchdog" 2>/dev/null || true
      set -e
      # TERM → 143, KILL → 137 under bash; also treat near-bound wall time as timeout.
      if [[ $rc -eq 143 || $rc -eq 137 || $rc -eq 130 ]]; then
        timed_out=1
      else
        end="$(now_s)"
        local wall
        wall="$(python3 -c 'import sys; print(float(sys.argv[2]) - float(sys.argv[1]))' "$start" "$end")"
        if [[ $rc -ne 0 ]] && python3 -c "import sys; sys.exit(0 if float(sys.argv[1]) >= float(sys.argv[2]) - 1.5 else 1)" "$wall" "$timeout_secs"; then
          timed_out=1
        fi
      fi
    fi
  else
    set +e
    "$@" >"$out_file" 2>&1
    rc=$?
    set -e
  fi
  end="$(now_s)"

  _RUN_OUT="$(cat "$out_file")"
  _RUN_RC="$rc"
  _RUN_TIMED_OUT="$timed_out"
  _RUN_S="$(elapsed_s "$start" "$end")"
  rm -f "$out_file"
}

# Parse TAP-ish stdout. Sets _TAP_OK (0|1).
parse_tap() {
  local out="$1"
  local exit_rc="$2"
  _TAP_OK=0
  if [[ "$exit_rc" -ne 0 ]]; then
    return
  fi
  if printf '%s\n' "$out" | grep -qE '^not ok '; then
    return
  fi
  if ! printf '%s\n' "$out" | grep -qE '^ok [0-9]+ -'; then
    return
  fi
  local summary
  summary="$(printf '%s\n' "$out" | grep -E '^# [0-9]+/[0-9]+ passed' | tail -n1 || true)"
  if [[ -z "$summary" ]]; then
    return
  fi
  local passed total
  passed="$(printf '%s' "$summary" | sed -E 's/^# ([0-9]+)\/([0-9]+) passed.*/\1/')"
  total="$(printf '%s' "$summary" | sed -E 's/^# ([0-9]+)\/([0-9]+) passed.*/\2/')"
  if [[ "$passed" == "$total" && "$total" -gt 0 ]]; then
    _TAP_OK=1
  fi
}

read_knobs() {
  local src="$1"
  _N="$(grep -E '^N = [0-9]+' "$src" | head -n1 | sed -E 's/^N = //' || true)"
  _M="$(grep -E '^M = [0-9]+' "$src" | head -n1 | sed -E 's/^M = //' || true)"
  [[ -n "$_N" ]] || _N="?"
}

prepare_source() {
  local program="$1"
  local orig="${ROOT}/bench/${program}.sudo"
  if [[ ! -f "$orig" ]]; then
    echo "error: missing bench program: $orig" >&2
    exit 2
  fi
  if [[ -z "$SIZE_OVERRIDE" ]]; then
    _SRC="$orig"
    return
  fi
  # Module name == file stem, so keep the stem identical to the original
  # program (e.g. list_append.sudo). Stage under a size-specific subdir.
  local tmpdir="${WORK_ROOT}/size_${SIZE_OVERRIDE}_${program}"
  mkdir -p "$tmpdir"
  local tmp="${tmpdir}/${program}.sudo"
  # Only rewrite the module-level `N = <digits>` line; leave comments alone.
  sed -E "s/^(N = )[0-9]+/\1${SIZE_OVERRIDE}/" "$orig" >"$tmp"
  _SRC="$tmp"
}

exec_recipe_step() {
  # exec_recipe_step <cwd> <recipe.json> build|run [index]
  local cwd="$1"
  local recipe="$2"
  local kind="$3"
  local index="${4:-0}"
  (
    cd "$cwd"
    python3 -c '
import json, sys, os
r = json.load(open(sys.argv[1]))
kind = sys.argv[2]
idx = int(sys.argv[3])
if kind == "build":
    argv = r["build"][idx]
else:
    argv = r["run"]
os.execvp(argv[0], argv)
' "$recipe" "$kind" "$index"
  )
}

emit_row() {
  local program="$1" backend="$2" n="$3" m="$4" build_s="$5" run_s="$6" total_s="$7" status="$8"
  local row
  row="BENCH program=${program} backend=${backend} n=${n}"
  if [[ -n "$m" ]]; then
    row+=" m=${m}"
  fi
  row+=" build_s=${build_s} run_s=${run_s} total_s=${total_s} status=${status}"
  printf '%s\n' "$row"
}

finish_run_row() {
  local program="$1" backend="$2" n="$3" m="$4" build_s="$5"
  local run_s="$_RUN_S"
  local total_s
  total_s="$(sum_s "$build_s" "$run_s")"

  if [[ "$_RUN_TIMED_OUT" -eq 1 ]]; then
    emit_row "$program" "$backend" "$n" "$m" "$build_s" "$run_s" "$total_s" "timeout"
    return
  fi

  parse_tap "$_RUN_OUT" "$_RUN_RC"
  if [[ "$_TAP_OK" -eq 1 ]]; then
    emit_row "$program" "$backend" "$n" "$m" "$build_s" "$run_s" "$total_s" "ok"
  else
    emit_row "$program" "$backend" "$n" "$m" "$build_s" "$run_s" "$total_s" "fail"
    ANY_FAIL=1
    printf '%s\n' "--- fail detail program=$program backend=$backend ---" >&2
    printf '%s\n' "$_RUN_OUT" >&2
    printf '%s\n' "--- exit=$_RUN_RC ---" >&2
  fi
}

bench_intree() {
  local program="$1"
  local backend="$2"
  local src="$3"

  local outdir="${WORK_ROOT}/${program}_${backend}_out"
  local stage="${WORK_ROOT}/${program}_${backend}_stage"
  rm -rf "$outdir" "$stage"
  mkdir -p "$outdir" "$stage"

  read_knobs "$src"
  local n="$_N" m="$_M"

  # 1. Codegen with tests baked in.
  "$SUDOC" build --target "$backend" --tests -o "$outdir" "$src" >/dev/null

  # 2. Canonical build+run recipe from sudoc (entry already substituted).
  local recipe="${stage}/recipe.json"
  "$SUDOC" emit-recipe --target "$backend" -o "$recipe" "$src"

  # 3. Build steps (if any), timed separately.
  local build_s="0.00"
  local nbuild build_start build_end
  nbuild="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["build"]))' "$recipe")"
  if [[ "$nbuild" -gt 0 ]]; then
    build_start="$(now_s)"
    local i
    for ((i = 0; i < nbuild; i++)); do
      if ! exec_recipe_step "$outdir" "$recipe" build "$i"; then
        emit_row "$program" "$backend" "$n" "$m" "0.00" "0.00" "0.00" "fail"
        ANY_FAIL=1
        return
      fi
    done
    build_end="$(now_s)"
    build_s="$(elapsed_s "$build_start" "$build_end")"
  fi

  # 4. Run step, timed (optionally bounded).
  local -a run_argv=()
  while IFS= read -r -d '' arg; do
    run_argv+=("$arg")
  done < <(python3 -c 'import json,sys
for a in json.load(open(sys.argv[1]))["run"]:
    print(a, end="\0")
' "$recipe")

  local oldpwd="$PWD"
  cd "$outdir"
  if [[ -n "$TIMEOUT_SECS" ]]; then
    run_step "$TIMEOUT_SECS" "${run_argv[@]}"
  else
    run_step "" "${run_argv[@]}"
  fi
  cd "$oldpwd"

  finish_run_row "$program" "$backend" "$n" "$m" "$build_s"
}

bench_hs() {
  local program="$1"
  local src="$2"

  local stage="${WORK_ROOT}/${program}_hs_stage"
  local outdir="${WORK_ROOT}/${program}_hs_out"
  rm -rf "$stage" "$outdir"
  mkdir -p "$stage" "$outdir"

  read_knobs "$src"
  local n="$_N" m="$_M"

  # 1. emit-ir
  "$SUDOC" emit-ir -o "${stage}/modules.json" "$src"

  # 2. protocol-3 emit request envelope (lockstep.bzl _external_codegen_impl)
  printf '{"protocol":3,"cmd":"emit","entry":"%s","with_tests":true,"modules":' "$program" >"${stage}/request.json"
  cat "${stage}/modules.json" >>"${stage}/request.json"
  printf '}' >>"${stage}/request.json"

  # 3. emit.sh (stderr → log; pre-existing GHC partial-function noise from Emit.hs)
  set +e
  "$HS_EMIT" <"${stage}/request.json" >"${stage}/response.json" 2>"${stage}/emit.stderr"
  local emit_rc=$?
  set -e
  if [[ $emit_rc -ne 0 ]]; then
    echo "error: hs emit failed for $program (rc=$emit_rc); see ${stage}/emit.stderr" >&2
    emit_row "$program" "hs" "$n" "$m" "0.00" "0.00" "0.00" "fail"
    ANY_FAIL=1
    return
  fi

  # 4. unpack into outdir → <program>_test.hs, SudoRt.hs, T_<Camel>.hs
  "$EMIT_UNPACK" -o "$outdir" <"${stage}/response.json"

  # 5. Build with EXACT recipe_build from backends/haskell/BUILD.bazel
  #    ghc -O0 -rtsopts -with-rtsopts=-K8m -o {entry}_test {entry}_test.hs
  local build_start build_end build_s
  build_start="$(now_s)"
  set +e
  (
    cd "$outdir"
    ghc -O0 -rtsopts -with-rtsopts=-K8m -o "${program}_test" "${program}_test.hs" \
      >"${stage}/ghc.stdout" 2>"${stage}/ghc.stderr"
  )
  local ghc_rc=$?
  set -e
  build_end="$(now_s)"
  build_s="$(elapsed_s "$build_start" "$build_end")"
  if [[ $ghc_rc -ne 0 ]]; then
    echo "error: ghc build failed for $program (rc=$ghc_rc)" >&2
    cat "${stage}/ghc.stderr" >&2 || true
    emit_row "$program" "hs" "$n" "$m" "$build_s" "0.00" "$build_s" "fail"
    ANY_FAIL=1
    return
  fi

  # 6. Run ./<program>_test
  local oldpwd="$PWD"
  cd "$outdir"
  if [[ -n "$TIMEOUT_SECS" ]]; then
    run_step "$TIMEOUT_SECS" "./${program}_test"
  else
    run_step "" "./${program}_test"
  fi
  cd "$oldpwd"

  finish_run_row "$program" "hs" "$n" "$m" "$build_s"
}

bench_one() {
  local program="$1"
  local backend="$2"
  prepare_source "$program"
  local src="$_SRC"

  if is_intree "$backend"; then
    bench_intree "$program" "$backend" "$src"
  elif [[ "$backend" == "hs" ]]; then
    bench_hs "$program" "$src"
  else
    echo "error: unknown backend: $backend" >&2
    exit 2
  fi
}

# --- CLI parse ---
while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --backends)
      shift
      [[ $# -gt 0 ]] || { echo "error: --backends needs a value" >&2; exit 2; }
      IFS=',' read -r -a BACKENDS <<<"$1"
      shift
      ;;
    --backends=*)
      IFS=',' read -r -a BACKENDS <<<"${1#*=}"
      shift
      ;;
    --size)
      shift
      [[ $# -gt 0 ]] || { echo "error: --size needs a value" >&2; exit 2; }
      SIZE_OVERRIDE="$1"
      shift
      ;;
    --size=*)
      SIZE_OVERRIDE="${1#*=}"
      shift
      ;;
    --timeout)
      shift
      [[ $# -gt 0 ]] || { echo "error: --timeout needs a value" >&2; exit 2; }
      TIMEOUT_SECS="$1"
      shift
      ;;
    --timeout=*)
      TIMEOUT_SECS="${1#*=}"
      shift
      ;;
    --)
      shift
      break
      ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if is_backend "$1"; then
        BACKENDS+=("$1")
      else
        PROGRAMS+=("$1")
      fi
      shift
      ;;
  esac
done

while [[ $# -gt 0 ]]; do
  if is_backend "$1"; then
    BACKENDS+=("$1")
  else
    PROGRAMS+=("$1")
  fi
  shift
done

if [[ ${#BACKENDS[@]} -eq 0 ]]; then
  BACKENDS=("${DEFAULT_BACKENDS[@]}")
fi
if [[ ${#PROGRAMS[@]} -eq 0 ]]; then
  PROGRAMS=("${DEFAULT_PROGRAMS[@]}")
fi

for b in "${BACKENDS[@]}"; do
  if ! is_backend "$b"; then
    echo "error: unknown backend: $b" >&2
    exit 2
  fi
done

for p in "${PROGRAMS[@]}"; do
  if [[ ! -f "${ROOT}/bench/${p}.sudo" ]]; then
    echo "error: no such program: bench/${p}.sudo" >&2
    exit 2
  fi
done

ensure_bins

WORK_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/sudo-bench.XXXXXX")"
cleanup() { rm -rf "$WORK_ROOT"; }
trap cleanup EXIT

echo "sudo list-ops bench  root=$ROOT"
echo "programs: ${PROGRAMS[*]}"
echo "backends: ${BACKENDS[*]}"
[[ -n "$SIZE_OVERRIDE" ]] && echo "size override: N=$SIZE_OVERRIDE"
[[ -n "$TIMEOUT_SECS" ]] && echo "run timeout: ${TIMEOUT_SECS}s"
echo "work: $WORK_ROOT"
echo

for program in "${PROGRAMS[@]}"; do
  for backend in "${BACKENDS[@]}"; do
    bench_one "$program" "$backend"
  done
done

echo
if [[ "$ANY_FAIL" -ne 0 ]]; then
  echo "DONE with correctness failures" >&2
  exit 1
fi
echo "DONE"
exit 0
