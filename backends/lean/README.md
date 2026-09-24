# Lean 4 backend (external, protocol 4)

A `sudo_external_backend` that emits Lean 4 source from the protocol-4 JSON
IR. It is registered as `//backends/lean:lean` and is **not** a member of
`ALL_BACKENDS`. v1 is terminates-first: usable for proofs and KAT-style TAP
checks on programs that the `terminates` profile accepts *and* that this
emitter can lower to total Lean defs.

## Toolchain

- **Emitter:** Python ≥ 3.10 (stdlib only). No extra CI dependency.
- **Generated code:** Lean **4.14.x**, no Mathlib. Pinned in `lean-toolchain`
  as `leanprover/lean4:v4.14.0`.
- **Install:** [elan](https://github.com/leanprover/elan)

  ```sh
  curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf \
    | sh -s -- -y --default-toolchain leanprover/lean4:v4.14.0
  ```

## How to build and run

```sh
# protocol-4 parse / refuse tests (no Lean required)
bazel test //backends/lean:emit_protocol_test

# lockstep subset vs py (needs Lean 4.14 on PATH)
bazel test //backends/lean:all
# or, from the dedicated CI job:
tools/ci-bazel.sh test //backends/lean:all
```

The lockstep leaves are tagged `lean`. The default compiler CI runs

```sh
tools/ci-bazel.sh test //... --test_tag_filters=-lean
```

so existing backends stay green without a Lean install.

### Manual emit (no Bazel)

```sh
bazel build //sudoc/crates/cli:sudoc
SUDOC=bazel-bin/sudoc/crates/cli/sudoc
$SUDOC emit-ir --require terminates -o /tmp/modules.json backends/lean/examples/sum.sudo
printf '{"protocol":4,"cmd":"emit","entry":"sum","with_tests":true,"modules":' > /tmp/req.json
cat /tmp/modules.json >> /tmp/req.json
printf '}' >> /tmp/req.json
python3 backends/lean/emit.py < /tmp/req.json > /tmp/resp.json
# unpack files, then:
lean --run sum_test.lean
```

`predicates = ["terminates"]` is applied by `sudoc emit-ir --require terminates`
**before** the envelope is built. The emitter does not read the predicate
(backend-guide.md: profiles are a frontend gate).

## What the emitter does

- Strict protocol-4 parse: reject unknown versions, fields, and IR tags.
- Emit `sudo_types` and resolve program-unique nominal symbols via the
  program-wide decl table (v0.7 / protocol 4).
- `for` / `for-in` → `SudoRt.forRange` / `forInArr` (total Nat-fueled).
- Traps → `SudoM` (`EStateM Trap Unit`); TAP `ok` / `not ok [Kind: detail]`.
- Maps/Sets → sorted association arrays (`SOrd` key order). Documented
  deterministic choice; sudo still treats observable order-dependence as a bug.

## What it refuses

`{"error": "..."}` rather than `partial` / `sorry` / opaque loops:

- `while` (decreases is **not** on the IR, so a Lean measure cannot be rebuilt)
- recursive / mutually recursive functions

See [STATUS.md](STATUS.md) for the green subset vs OPEN corpus gaps.
