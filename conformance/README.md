# The conformance suite

`semantics/` is the executable form of the language spec: nine sudo modules
whose tests pin every observable behavior — value semantics, overflow traps,
IEEE float edges, loop control (including `break` crossing `match` and the
INT64_MAX range edge), the trap surface, structural map keys, inout hoisting
order, monomorphized generics, and text-as-scalars.

```console
$ sudoc conformance                      # all registered backends, lockstep
$ sudoc conformance --target zig         # one backend vs. itself (fast iteration)
```

A backend conforms when every module here passes **and agrees across all
backends** under the lockstep harness. Order-dependence is the one sanctioned
divergence axis (see `examples/pitfalls/order_dependent.sudo` for the
deliberate demonstration — it is intentionally *not* in this directory).

`golden/` holds typed-IR dumps of the examples, used by the compiler's
golden-file tests (`BLESS=1 cargo test -p sudoc-types --test golden` to
regenerate after reviewing a frontend change).

New semantic guarantees land as new modules here, in the same commit as the
spec change they enforce.
