# The conformance suite

`semantics/` is the executable form of the language spec: two dozen sudo modules
whose tests pin every observable behavior — value semantics, overflow traps,
IEEE float edges, loop control (including `break` crossing `match` and the
INT64_MAX range edge), the trap surface, structural map keys, inout hoisting
order, monomorphized generics, and text-as-scalars.

```console
$ bazel test //conformance/...           # every module, all seven backends, lockstep
$ bazel test //conformance:traps         # one module across all backends (fast iteration)
```

Each module is a `sudo_lockstep_test`: codegen + per-backend recipe are cached
build actions, and the run-leaves execute every backend and diff outcomes.

A backend conforms when every module here passes **and agrees across all
backends** under the lockstep harness. Order-dependence is the one sanctioned
divergence axis (see `examples/pitfalls/order_dependent.sudo` for the
deliberate demonstration — it is intentionally *not* in this directory).

`golden/` holds typed-IR dumps of the examples, used by the compiler's
golden-file test (`//sudoc/crates/types:golden`). Regenerate after reviewing a
frontend change by running that test's binary with `BLESS=1` (a local run that
writes back to the source tree).

New semantic guarantees land as new modules here, in the same commit as the
spec change they enforce.
