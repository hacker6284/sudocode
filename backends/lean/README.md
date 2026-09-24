# Lean 4 external backend

Protocol-4 wire emitter: JSON IR on stdin, Lean 4 source on stdout. Hosted
next to [`backends/haskell/`](../haskell/) and registered with the same
`sudo_external_backend` rule.

**Status: unfinished / not a lockstep peer.** `//backends/lean:lean` exists so
the descriptor can be referenced by label, but it is **not** in
`ALL_BACKENDS` (`tools/backends.bzl`). Root README badges are unchanged.
Registering a backend with empty `predicates` makes it a full peer: every
conformance / stdlib / examples lockstep module — including those that do
not pass `--require terminates` — must agree with the reference backends.
This PR does not opt out of that gate with `predicates = ["terminates"]`.
Until the required suite is green, Lean stays behind this unfinished
target (option (a) in the landing brief).

## Toolchain

| Piece | Version |
|---|---|
| Lean | 4.14.0 (`leanprover/lean4:v4.14.0`) |
| Lake | ships with that Lean (5.x) |
| Emitter | Python 3 (stdlib `json` only — no Lean on the codegen PATH) |
| Mathlib | **not used** |

Install via [elan](https://github.com/leanprover/elan):

```bash
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
  → recipe_build: lake build
  → recipe_run:   ./.lake/build/bin/{entry}_test
  → capture_run → lockstep_diff
```

To force the totality profile *by hand* (not how this backend is
registered):

```bash
sudoc emit-ir --tests --require terminates path/to/mod.sudo
```

A program that uses a refused `export` function fails that command with
`RefusedExport`. Tests the checker refuses are stripped; the rest still
have to lockstep.

Once Lean is in `ALL_BACKENDS`, the gate is:

```bash
bazel test //conformance:all //stdlib:all //examples:all
```

Until then, a one-off lockstep against this descriptor looks like
editing a single `dogfood_lockstep_test` to pass
`backends = ALL_BACKENDS + ["//backends/lean:lean"]` locally — do **not**
land that until the suite is green.

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
| `emit.sh` | cd to runfiles; `exec python3 emit.py` |
| `emit.py` | strict protocol-4 parse + Lean 4 emit |
| `SudoRt.lean` | traps, i64/float, Array lists, SMap/SSet, Canon, TAP runner |

## What is green / what remains

Tracked in the PR body. At land time the emitter + runtime + descriptor
are in-tree; the ALL_BACKENDS / README-badge registration is **held**
until `//conformance:all`, `//stdlib:all`, and `//examples:all` agree
with the reference backends when Lean is added to that list.
