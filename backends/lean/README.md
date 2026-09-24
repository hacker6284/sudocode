# Lean 4 external backend

Protocol-4 wire emitter: JSON IR on stdin, Lean 4 source on stdout. Hosted
next to [`backends/haskell/`](../haskell/) and registered with the same
`sudo_external_backend` rule.

**Status: lockstep peer.** `//backends/lean:lean` is in `ALL_BACKENDS`
(`tools/backends.bzl`). Empty `predicates` (full IR, including `while`).
`predicates = ["terminates"]` is not used. Default `bazel test //...`
runs Lean with the other seven peers and needs `lake` on PATH
(`tools/ci-elan.sh` after `bazel build`, same split as Swift).

The emitter is trusted-not-proved: lockstep agreement is the bar, not a
sudo↔Lean semantic-equivalence proof. No cryptographic-strength claim.

Darwin run-leaf (keep these; they cleared the macOS canary): generated
`build_test.sh` bakes LC_RPATH / collocates `@rpath` dylibs; lakefile
passes `-Wl,-rename_segment,__DATA_CONST,__DATA` on `System.Platform.isOSX`;
unsigned `capture_run` applies `SUDO_LOADER_LIBS` as `DYLD_*` /
`LD_LIBRARY_PATH`; failed run-leaves print stderr.

## Toolchain

| Piece | Version |
|---|---|
| Lean | 4.14.0 (`leanprover/lean4:v4.14.0`) |
| Lake | ships with that Lean (5.x) |
| Emitter | Python 3 (stdlib `json` only — no Lean on the codegen PATH) |
| Mathlib | **not used** |

Install via [elan](https://github.com/leanprover/elan), or the CI helper:

```bash
tools/ci-elan.sh
# or:
curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf | sh
elan toolchain install leanprover/lean4:v4.14.0
elan default leanprover/lean4:v4.14.0
```

Generated trees pin `lean-toolchain` to `leanprover/lean4:v4.14.0`. The
emitter itself is `emit.sh` → `python3 emit.py` and only needs Python 3.

## How `terminates` interacts with lockstep (current main)

These are orthogonal layers:

- **Frontend gate.** `sudoc emit-ir --require terminates` (and
  `emit-skips --require`) drops tests the totality checker refuses and
  fails codegen on a `RefusedExport` (a refused `export` function). This
  is *not* a wire field; the protocol-4 envelope is unchanged.
- **Backend predicates.** `sudo_external_backend(..., predicates = ["terminates"])`
  is the only way an external backend gets `--require` on emit-ir /
  emit-skips. An empty list (Haskell, and this backend's declared attrs)
  is a **full peer**: the emitter sees the complete IR, including `while`.
- **Lockstep.** `dogfood_lockstep_test` defaults to `ALL_BACKENDS`. A
  registered backend must match the reference backends test-for-test.
  Skips are not a vote; a missing or divergent TAP line fails the gate.

So: Lean without a predicate means every lockstep module that runs
Haskell must also run Lean and agree. That is the bar. This tree does
not ship a half-registered backend that opts out.

`while` / `for` are lowered to **total** `let rec` on a `Nat` fuel
(for-range / for-in: remaining-iteration count; while: `2^32`). The
emitter refuses `partial`, `sorry`, and opaque loops. Fuel exhaustion is
the `StackOverflow` trap, not a silent hang. A full deep embedding / fuel
interpreter for *non-terminating* sudo, and any claim of sudo↔Lean
semantic-equivalence proofs, are out of scope.

## sudoc / Bazel invocation

Emit IR (no totality filter — full peer, same as Haskell):

```bash
sudoc emit-ir --tests conformance/semantics/arithmetic.sudo > /tmp/arith.ir.json
# wrap in the protocol-4 envelope {protocol:4, cmd:emit, entry, with_tests, modules}
python3 backends/lean/emit.py < /tmp/arith.req.json > /tmp/arith.files.json
```

The Bazel codegen action (what lockstep actually runs) is:

```text
sudoc emit-ir --tests <entry>     # no --require; predicates = []
  → envelope (protocol 4)
  → //backends/lean:emitter
  → emit_unpack
  → recipe_build: /bin/bash ./build_test.sh   # lake build + Darwin LC_RPATH
  → recipe_run:   ./.lake/build/bin/{entry}_test
  → capture_run (SUDO_LOADER_LIBS → DYLD_*/LD_LIBRARY_PATH on the child)
  → lockstep_diff
```

To force the totality profile *by hand* (not how this backend is
registered):

```bash
sudoc emit-ir --tests --require terminates path/to/mod.sudo
```

A program that uses a refused `export` function fails that command with
`RefusedExport`. Tests the checker refuses are stripped; the rest still
have to lockstep.

The eight-peer gate is the default suite (needs the seven other
toolchains **and** `lake` on PATH):

```bash
bazel test //conformance:all //stdlib:all //examples:all
# or:
tools/lockstep
```

## Lean shape

- Generated core is total Lean 4.14; no Mathlib.
- `Except SudoRt.Trap` is the trap monad. Traps become TAP `not ok`
  lines with `[Kind]` / `[Kind: detail]`, matching
  `spec/lockstep.md` / the Haskell runner. TAP names are
  `test_*` (`sudoc_ir::names::test_fn_names`), not the human test titles.
- Each sudo module is a Lean file + matching `namespace` (Lake modules
  do not automatically namespace top-level decls).
- `sudo_types` (protocol 4 / v0.7) is emitted like any other module;
  nominal homes go through the program-wide decl table (`typeHome` /
  `qualNominal`).
- **Maps / sets:** sorted association lists (`SMap` / `SSet`) ordered by
  `SOrd`. Iteration is key-sorted. Equality is order-insensitive. This is
  one legal choice under sudo's unspecified-order rule; observable tests
  that read "the first iterated key" will see the least key, not
  insertion order.
- i64 is Lean `Int` narrowed to `[-2^63, 2^63)`. Floor div/mod via
  `Int.fdiv` / `Int.fmod`. Floats are IEEE (NaN ≠ NaN, signed zero,
  ties-away-from-zero `Float.round`).
- `inout` is writeback-by-return. `MutBuiltin` is hoisted to statements
  before expression emit.

## Layout

| File | Role |
|---|---|
| `BUILD.bazel` | `sh_binary` emitter + `sudo_external_backend(name = "lean")` |
| `build_test.sh` (generated) | `lake build` + Darwin `install_name_tool` LC_RPATH / `@loader_path` dylibs |
| `emit.sh` | cd to runfiles; `exec python3 emit.py` |
| `emit.py` | strict protocol-4 parse + Lean 4 emit |
| `SudoRt.lean` | traps, i64/float, Array lists, SMap/SSet, Canon, TAP runner |

## What is green / what remains

**In `ALL_BACKENDS`.** Empty `predicates` (full IR). Root badge includes
`lean`. Default `bazel test //...` is the eight-peer gate.

| Area | Status |
|---|---|
| Measured surface (semantics 30 + stdlib 4 + examples 9 + multimodule 14) | **Linux + macOS CI-green** as the #7 canary (57/57). Now the same surface is the default lockstep list. |
| Darwin run-leaf | LC_RPATH / `@loader_path`, `SUDO_LOADER_LIBS`, `-Wl,-rename_segment,__DATA_CONST,__DATA`, ad-hoc codesign. Do not regress. |
| Emitter soundness | Trusted-not-proved. Lockstep agreement, not a Lean proof of sudo semantics. |

`while`/`for` lower to `SudoRt.natIter` (fuel-total). No `partial` / `sorry`.
Flow payload binders are `_fs`, never `s` — a sudo `for s` index is also
mangled to `s`, and the old `| .brk s` / `| .cont s` arms either failed
`lake` (MegaDreifach) or compiled and computed the wrong sum.
