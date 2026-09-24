# Lean backend status

Head of this work is recorded on the PR. Re-check after rebase:

```sh
git rev-parse HEAD
bazel test //backends/lean:emit_protocol_test
bazel test //backends/lean:all          # needs Lean 4.14
```

## Green (v1 target)

| Target | Why it should work |
|---|---|
| `//backends/lean:emit_protocol_test` | protocol 4 parse, while-refuse, version reject; no Lean |
| `//backends/lean:sum` | `examples/sum.sudo` — `for`-range only |
| `//backends/lean:totality` | `conformance/predicates/totality.sudo` — `sum_to` emits; `while_break` is a `terminates` skip |
| `//backends/lean:arithmetic` | i64 overflow / `expect_trap` observe-mode |
| `//backends/lean:traps` | expect_trap + containers, no while |
| `//backends/lean:module_constants` | consts + `for-in`, no while |
| `//backends/lean:floats` | IEEE helpers + float `(-x)` + `for`-range |

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
