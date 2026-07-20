# Contributing to sudocode

## Adding a backend (the headline contribution)

There are **two equal front doors** — pick per the rubric in
[backend guide §0](spec/backend-guide.md):

- **In-tree (Rust)**: implement `sudoc_sdk::Backend` (see
  `sudoc/crates/sdk`) in a new `sudoc/crates/backend_<lang>` crate and
  register it — one line in `sudoc_harness::all_backends()`, one workspace
  member. The Python backend is the friendliest reference for dynamic
  targets; C for manual-memory targets.
- **External (any language)**: implement the
  [wire protocol](spec/protocol.md) — a manifest plus an executable that
  reads typed IR as JSON and returns generated files. Drop it under
  `backends/<lang>/` and it auto-registers; `--target <name>` works like
  any built-in. The Haskell backend (`backends/haskell/`) is the reference.

Either way:

1. Read [`spec/backend-guide.md`](spec/backend-guide.md) first — the porting
   order and the land-mine catalog. Seven backends have already paid for
   those lessons; don't pay twice.
2. **Definition of done**: `sudoc conformance --target <yours>` green — your
   backend agrees with all reference backends on every module in
   `conformance/semantics/` — plus `cargo test --workspace` at zero failures
   and zero clippy warnings for in-tree work.
3. Write `notes/friction-<lang>.md` as you go: every place the guide, SDK,
   or protocol was unclear or wrong. Friction logs are how the docs improve —
   the existing ones are the expected caliber.

An independent implementation of an already-covered language is a valid
contribution too: register it under a distinct name and the lockstep
harness diffs it against the existing reference implementation.

## Changing the language or its semantics

The spec (`spec/language.md`) and the conformance corpus move together: a
semantic change lands with its corpus module, and all backends must re-agree.
Sudo has no implementation-defined behavior — if a proposal introduces
"backends may choose," it needs a redesign. Where mainstream languages
disagree (precedence, rounding, iteration order), sudo either pins one
defined behavior or refuses to guess (mandatory parentheses, unspecified
order + lockstep detection).

## Everything else

- `cd sudoc && cargo test --workspace && cargo clippy --all-targets` before
  pushing; CI enforces both plus the conformance suite.
- Generated-code readability is a product goal, not a nicety — diffs that
  make emitted code uglier need a correctness reason.
- Comments state invariants and constraints, not narration.
