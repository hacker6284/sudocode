# List-ops performance harness

Wall-clock benchmarks for `List<T>` operations that are O(n) per call in
the Haskell runtime (`backends/haskell/SudoRt.hs`). The goal is a
**reproducible baseline** so a future representation change (e.g.
`Data.Sequence`) can be justified with numbers instead of anecdote.

## Design: plain script, not a Bazel target

`bench/run.sh` is a standalone bash script. Timing is taken around the
**compiled/interpreted artifact itself** (the recipe's `run` argv), not
around `bazel test` — Bazel action caching would make "run time"
meaningless for a perf harness. Codegen reuses sudoc's own mechanisms
(`sudoc build` / `emit-recipe` for in-tree backends; protocol-4
`emit-ir` → `backends/haskell/emit.sh` → `emit_unpack` → the exact
`recipe_build` from `backends/haskell/BUILD.bazel` for hs). No Bazel
target is registered, so `bazel test //...` is untouched.

## How to run

From the repo root (requires `bazel-bin/sudoc/crates/cli/sudoc` and
`bazel-bin/sudoc/crates/harness/emit_unpack`; the script builds them via
Bazel if missing; hs also needs `ghc`/`runghc` on `PATH`):

```bash
# Default: both programs × py,hs at committed sizes
./bench/run.sh

# One program / backend
./bench/run.sh list_append hs
./bench/run.sh --backends py,hs list_indexed

# Override module-level N without editing the committed .sudo
./bench/run.sh --size 1000000 list_append py

# Bound only the run step (timeout → status=timeout, script continues, exit 0)
./bench/run.sh --size 1000000 --timeout 120 list_append hs
```

Machine-parseable rows are lines starting with `BENCH` (greppable with
`grep '^BENCH'`). Tokens: `program=`, `backend=`, `n=`, optional `m=`,
`build_s=`, `run_s=`, `total_s=`, `status=` (`ok` | `timeout` | `fail`).

`--timeout` uses GNU `timeout` or Homebrew `gtimeout` when present;
otherwise a bash watchdog (and a one-time warning). Non-timeout
correctness failures (nonzero exit, `not ok`, mismatched pass counts)
make the script exit 1; pure timeouts do not.

Other in-tree backends (`js`, `c`, `rs`, `zig`, `swift`) are accepted
via `--backends` / positionals but are not required for the baseline.

## What each program isolates

### `list_append.sudo`

N sequential `xs.append(i)` into an initially empty `List<int>`, then a
length + sum check. Isolates `appendL` in
`backends/haskell/SudoRt.hs:243-245` (`xs ++ [v]`), which is O(n) per
call and makes the whole loop O(n²). This is the scaled-down shape of
the F11 anecdote in `notes/external-review-2026-07-28.md` ("1M appends
into an inout list").

**Default:** `N = 20000` (finishes in a few seconds on py and hs).

### `list_indexed.sudo`

N indexed reads (`xs[idx]`) and writes (`xs[idx] = v + 1`) over a fixed
list of length M, round-robin. Isolates `at` (`SudoRt.hs:233-236`, `!!`
plus a length recompute) and `putL` (`SudoRt.hs:238-241`,
`take`/`++`/`drop`) — both O(n) per call on the plain-list
representation. The final sum of the list is exactly N, independent of
M (a cheap order-independent correctness check).

**Defaults:** `N = 100000`, `M = 1000`.

## F11-scale reproduction (N = 1,000,000)

Do **not** raise the committed default to this size — on hs it will not
finish cleanly for a long time (or will StackOverflow under the
recipe's `-with-rtsopts=-K8m` stack bound; see baseline below).

```bash
# Bounded run: demonstrates non-success at F11 size without hanging the shell
./bench/run.sh --size 1000000 --timeout 120 list_append hs

# For comparison, py at the same size is fine:
./bench/run.sh --size 1000000 list_append py
```

Naive quadratic extrapolation from ~N=100k ≈ 100s run time on hs would
put N=1M around ~10,000s; a 60–180s `--timeout` is enough to show
non-completion *if* the process stays alive that long. With the current
hs recipe (`-with-rtsopts=-K8m`, from `backends/haskell/BUILD.bazel`),
a single `xs ++ [v]` also needs O(n) stack, so N=1M typically hits
`StackOverflow` well before the wall-clock timeout — still a hard
failure of the plain-list representation at F11 scale.

## Baseline (2026-08-08)

One-machine measurement for **relative** comparison (hs vs py; this-N
vs that-N). Not an absolute or portable performance claim.

**Machine / toolchain**

| Item | Value |
|------|-------|
| `uname -a` | `Darwin Zachs-MacBook-Air.local 25.4.0 Darwin Kernel Version 25.4.0: Thu Mar 19 19:31:09 PDT 2026; root:xnu-12377.101.15~1/RELEASE_ARM64_T8132 arm64` |
| `ghc --version` | The Glorious Glasgow Haskell Compilation System, version 9.14.1 |
| `python3 --version` | Python 3.11.12 |

**Default sizes** (`./bench/run.sh`)

| program | backend | n | m | build_s | run_s | status |
|---------|---------|------:|------:|--------:|------:|--------|
| list_append | py | 20000 | — | 0.00 | 0.77 | ok |
| list_append | hs | 20000 | — | 1.85 | 3.77 | ok |
| list_indexed | py | 100000 | 1000 | 0.00 | 0.66 | ok |
| list_indexed | hs | 100000 | 1000 | 1.32 | 2.39 | ok |

At these sizes hs is already ~5× slower than py on run time for both
programs, consistent with O(n) list ops plus GHC `-O0` build cost.
(Numbers move a bit run-to-run; the relative shape is stable.)

**F11-scale** (`./bench/run.sh --size 1000000 --timeout 120 list_append hs`)

| program | backend | n | build_s | run_s | status | notes |
|---------|---------|--------:|--------:|------:|--------|-------|
| list_append | hs | 1000000 | 1.30 | 1.86 | **fail** | `not ok … [StackOverflow]` under recipe `-K8m`; did not reach the 120s wall timeout |
| list_append | py | 1000000 | 0.00 | 1.46 | ok | same N for contrast (`--size 1000000 list_append py`) |

So at the exact F11 size: py finishes in ~1.5s; hs does not produce a
passing result (StackOverflow from O(n) stack in `appendL` /
`xs ++ [v]` under the bounded RTS stack). That is independent evidence
for the same class of problem F11 recorded (1M sequential appends
unusable on hs, fine on py) — today's failure mode is stack rather than
an unbounded multi-minute hang, because the partial F11 fix
(`-with-rtsopts=-K8m`) bounds the stack.

Re-run the tables on your machine with the commands above; numbers will
move, the relative shape should not.

## Post-change: Data.Sequence (2026-08-08)

After F11: Haskell `List<T>` / `text` representation swapped from plain
`[a]` to `Data.Sequence.Seq` (`backends/haskell/{Emit,SudoRt}.hs`). Same
machine / toolchain as the baseline above; recipe flags unchanged
(`ghc -O0 -rtsopts -with-rtsopts=-K8m`).

**Default sizes** (`./bench/run.sh`)

| program | backend | n | m | build_s | run_s | status |
|---------|---------|------:|------:|--------:|------:|--------|
| list_append | py | 20000 | — | 0.00 | 0.36 | ok |
| list_append | hs | 20000 | — | 0.97 | 0.63 | ok |
| list_indexed | py | 100000 | 1000 | 0.00 | 0.38 | ok |
| list_indexed | hs | 100000 | 1000 | 0.96 | 0.79 | ok |

**N=40000 list_append hs** (`./bench/run.sh --size 40000 list_append hs`)

| program | backend | n | build_s | run_s | status |
|---------|---------|------:|--------:|------:|--------|
| list_append | hs | 40000 | 0.87 | 0.70 | ok |

(Pre-change this was ~10.6s run and ~quadrupled when N doubled 20k→40k;
post-change 40k is within noise of 20k — no longer quadratic.)

**F11-scale** (`./bench/run.sh --size 1000000 --timeout 120 list_append hs`)

| program | backend | n | build_s | run_s | status | notes |
|---------|---------|--------:|--------:|------:|--------|-------|
| list_append | hs | 1000000 | 1.02 | 1.86 | **ok** | previously `StackOverflow` under `-K8m`; now finishes in ~2s |

So at the exact F11 size: hs sequential append is tractable with
`Data.Sequence` (O(1) snoc per `appendL`), matching the class of fix F11
called for.
