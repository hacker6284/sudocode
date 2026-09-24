# Lean backend status

Verified at `58d2dcd07efe6d83b25bb5751e71badf2a4c1749` (this branch):

```sh
git rev-parse HEAD
# 58d2dcd07efe6d83b25bb5751e71badf2a4c1749
bazel test //backends/lean:emit_protocol_test   # PASSED (no Lean)
bazel test //backends/lean:all                  # needs Lean 4.14
# PASSED: arithmetic floats module_constants sum totality traps
```

## Green (v1 target)

| Target | Result |
|---|---|
| `//backends/lean:emit_protocol_test` | PASSED — protocol 4 parse, while-refuse, version reject |
| `//backends/lean:sum` | PASSED — `examples/sum.sudo` lockstep vs py |
| `//backends/lean:totality` | PASSED — `sum_to` emits; `while_break` is a `terminates` skip |
| `//backends/lean:arithmetic` | PASSED — i64 floor div/mod + overflow `expect_trap` |
| `//backends/lean:traps` | PASSED — observe-mode `expect_trap` + containers |
| `//backends/lean:module_constants` | PASSED — consts + `for-in` |
| `//backends/lean:floats` | PASSED — IEEE helpers + float `(-x)` + `for`-range |

These lockstep against **py** only (`backends = ["py", "//backends/lean:lean"]`).
They are **not** `//conformance:all` and Lean is **not** in `ALL_BACKENDS`.

## OPEN — not claimed

- **Full `bazel test //conformance/...` lockstep** with Lean in `ALL_BACKENDS`.
  Most semantics/examples modules use `while` without a measure on the wire,
  or recursion (`gcd`, `binary_search`, `loops`, stdlib sorts). Adding Lean
  to `ALL_BACKENDS` would fail `emit-ir --require terminates` on refused
  exports **or** the emitter's while/recursion refuse.
- **sudo-Lean semantic equivalence proofs.** Generated `def`s are total
  Lean under `SudoM`; that is not a proof they match sudo's spec.
- **Deep embedding / fuel interpreter** of the whole IR. Non-goal.
- **In-tree Rust rewrite.** Non-goal.
- Remaining IR that parses but is lightly exercised here: inout writeback,
  func-pointer `CallValue`, multimodule `sudo_types`, generic monomorphs,
  `expect_trap` nested in loops, `break`/`continue` crossing `match`.
- Float/`Int.toFloat` host quirks if Lean 4.14's `Float` helpers differ from
  the Python oracle — see `notes/friction-lean.md` if a cell regresses.

## Invocation that matches this profile

```text
sudoc emit-ir --require terminates -o modules.json entry.sudo
# envelope: {"protocol":4,"cmd":"emit","entry":STEM,"with_tests":true,"modules":...}
python3 backends/lean/emit.py < request.json
lean --run ${STEM}_test.lean
```

Bazel: `sudo_external_backend(..., predicates = ["terminates"], recipe_run = ["lean", "--run", "{entry}_test.lean"])`.
