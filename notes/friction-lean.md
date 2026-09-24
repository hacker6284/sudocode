# Friction log — Lean 4 backend

Written while landing `backends/lean/`. Gaps in the guide / protocol that
cost time, and choices that would surprise the next person.

## Profiles are not on the wire

`terminates` is a frontend gate (`emit-ir --require terminates`). The emitter
must still parse and emit whatever survived. `decreases` is **erased** before
IR (language.md §5.2 / termination.rs). A Lean `termination_by` cannot be
reconstructed. That is why this backend refuses `while` and recursion instead
of emitting `partial def` / `sorry`.

## `sudo_types` is a real module

Protocol 4: a synthetic first module named `sudo_types` holds escaped
nominals. `imports` are unit deps, not source `import` lines. Resolve
`Ty::Record` / `Ty::Enum` via the program-wide decl table, not "declared in
the module being emitted."

## Map / Set order

The language leaves iteration order unspecified. Lean has no native
structural-key map in Init. This backend uses sorted association arrays
(`SOrd`). Deterministic, and lockstep will still catch user order-dependence
against py/js insertion order.

## `while` in Lean `Id` is not a total `def`

Do not port Python `while i < n` into the runtime as `Id.run do while`.
Use a `Nat` fuel parameter (`n+1` → `n`). Same pattern as `forRange`.

## TAP names

Lockstep keys are `test_fn_names` (`test_` + sanitized title), not the
human `IrTest.name`. Same port as Haskell `sanitizeTest`.

## `lean --run` and imports

`lean --run File.lean` does not build sibling modules unless `.olean`s are
on `LEAN_PATH`. The lockstep artifact is a single `{entry}_test.lean` with
`SudoRt` inlined. Separate `SudoRt.lean` + per-module files are also
emitted for proof/Lake use.
