# Lean 4 external backend

Protocol-4 wire emitter: JSON IR on stdin, Lean 4 source on stdout. Hosted
next to [`backends/haskell/`](../haskell/) and registered with the same
`sudo_external_backend` rule.

**Status: unfinished / not a lockstep peer.** `//backends/lean:lean` exists so
the descriptor can be referenced by label, but it is **not** in
`ALL_BACKENDS` (`tools/backends.bzl`). Root README badges are unchanged.
CI installs elan + Lean 4.14.0 *after* `bazel build` and runs a **canary**
(`//backends/lean/canary:all`) with
`backends = ALL_BACKENDS + ["//backends/lean:lean"]` — empty `predicates`,
no module skipped. Those targets are tagged `manual`, so
`bazel test //...` stays the seven-peer gate and does not require `lake`.
This tree does not opt out of the registration bar with
`predicates = ["terminates"]`.

## Merge vs peer registration

Two different bars. A merge of `backends/lean/` is **not** peer
registration.

### Green enough to merge (unfinished emitter)

Land this tree so a consumer (cryptoys) can pin a durable ref — ideally
`main`, or the merge commit — and run protocol-4 emit → Lean 4.14 without
waiting on lockstep.

| Must hold | Why |
|---|---|
| `//backends/lean:lean` + `:emitter` exist; empty `predicates` | Full IR (including `while`), same envelope as Haskell. |
| **Not** in `ALL_BACKENDS` | Adding Lean would make every `dogfood_lockstep_test` invoke `lake`. The canary is the honest eight-backend gate until that suite is CI-green. |
| Root target badge stays `py \| c \| js \| rs \| swift \| zig \| hs` | Badge = lockstep peers only. |
| Existing `bazel test //...` (seven peers) stays green | Canary targets are `tags=["manual"]`; `//...` does not require `lake`. |
| CI elan / Lean 4.14.0 / `lake` after `bazel build` | Same pattern as Swift: codegen is Python-only; `lake` is the run-leaf. |
| `_fs` Flow binders (never `s`) | `for s` must not shadow carried state (MegaDreifach / `sum_s(3) == 6`). |
| Local emit → `lake` → TAP green on the measured surface | Semantics 30/30, stdlib, examples `_MODULES`, multimodule 14/14. |

`predicates = ["terminates"]` is **not** an acceptable shortcut to land
or to register.

### Still OPEN before Lean is a lockstep peer

These block `ALL_BACKENDS` / badges, not CI canary wiring:

1. **The canary itself on GitHub Actions.** `//backends/lean/canary:all`
   is the measured surface (semantics 30 + stdlib 4 + examples 9 +
   multimodule 14) with `ALL_BACKENDS + lean`. It is executable in CI
   (`tools/ci-elan.sh` after `bazel build`, then
   `tools/ci-bazel.sh test //backends/lean/canary:all`). Until that job
   is green, Lean is not a peer.
2. Only then: add `//backends/lean:lean` to `ALL_BACKENDS` and update the
   root badge. Do not weaken lockstep. Do not register with
   `predicates = ["terminates"]`.

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

So: registering Lean without a predicate means every lockstep module that
today runs Haskell must also run Lean and agree. That is the bar. This
tree does not ship a half-registered backend that opts out.

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

The eight-backend canary (not registered) is:

```bash
tools/lockstep-lean
# same as:
bazel test //backends/lean/canary:all
```

That expands `dogfood_lean_canary_test` leaves with
`backends = ALL_BACKENDS + ["//backends/lean:lean"]`. Needs the seven
peer toolchains **and** `lake` on PATH.

Once Lean is in `ALL_BACKENDS`, the gate becomes the existing seven-peer
command (now eight):

```bash
bazel test //conformance:all //stdlib:all //examples:all
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
| `canary/BUILD.bazel` | `test_suite` over the measured-surface `*_lean` leaves |
| `build_test.sh` (generated) | `lake build` + Darwin `install_name_tool` LC_RPATH / `@loader_path` dylibs |
| `emit.sh` | cd to runfiles; `exec python3 emit.py` |
| `emit.py` | strict protocol-4 parse + Lean 4 emit |
| `SudoRt.lean` | traps, i64/float, Array lists, SMap/SSet, Canon, TAP runner |

## What is green / what remains

**Not in `ALL_BACKENDS`.** Empty `predicates` (full peer) once wired.
Root README badges unchanged. CI can run `lake`; registration waits on
the canary.

Local protocol-4 emit → `lake build` → TAP (Lean 4.14.0):

| Area | Status |
|---|---|
| `conformance/semantics/*` | TAP-green locally, 30/30 modules (`trap_strictness` 30/30; `std_imports` 2/2; `structures` 6/6; `place_matrix` 66/66) |
| `stdlib/{strings,sorting,regex,bigint}` | TAP-green (`strings` 54/54, `sorting` 27/27, `regex` 37/37, `bigint` 16/16). Recursive `Item`/`Atom` are mutual inductives; recursive sudo funcs get a `Nat` fuel argument. Self-recursive enums (`bst` `Tree`) use cyclic BEq/Repr instances. |
| `examples/*` (`BUILD` `_MODULES`) | TAP-green: gcd, palindrome, binary_search, insertion_sort, quicksort, two_sum, bfs, bst 3/3, quine |
| `conformance/multimodule/*` | TAP-green, all 14 fixtures (imports, xmod_inout, f8_collision, xmod_generics, nominal_grid 45/45, nominal_places, nominal_identity, nominal_diamond, nominal_diamond_gen, nominal_one_escape, nominal_export, nominal_helpers, sort_by_thing, sort_by_key_thing). NewRecord field names are local `mangle_field`s, not `Sudo_types.qual_field` (Lean would parse the dotted name as field `Sudo_types`). |
| Bazel `//backends/lean:lean` + `:emitter` | **builds** (`bazel query '//backends/lean:*'` lists both; `bazel build` of those two targets succeeded on Bazel 8.3.1) |
| CI elan / Lean 4.14.0 / `lake` | **wired** — `tools/ci-elan.sh` after `bazel build` on Linux and macOS (same split as Swift). |
| Bazel canary `//backends/lean/canary:all` | **Linux CI-green** (57/57). **macOS:** still OPEN at the `lake exe` / bash-wrapper tips (57/57 `lean no result` — SIP strips `DYLD_*` from `/bin/bash`). This tip bakes LC_RPATH, collocates `@rpath` dylibs, and has unsigned `capture_run` apply `SUDO_LOADER_LIBS` as `DYLD_*` on the child (rs-backend pattern). Run-leaf stderr is now printed. Not a registration claim. |
| `ALL_BACKENDS` + root badge | **not registered.** Do not add Lean until the canary is green on CI. Adding it today would make every default `dogfood_lockstep_test` require `lake` and break developers / `//...` without elan. |

`while`/`for` lower to `SudoRt.natIter` (fuel-total). No `partial` / `sorry`.
Flow payload binders are `_fs`, never `s` — a sudo `for s` index is also
mangled to `s`, and the old `| .brk s` / `| .cont s` arms either failed
`lake` (MegaDreifach) or compiled and computed the wrong sum.
