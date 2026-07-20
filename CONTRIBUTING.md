# Contributing to sudocode

## Adding a backend (the headline contribution)

1. Read [`spec/backend-guide.md`](spec/backend-guide.md) — the porting order
   and the land-mine catalog. It exists because six backends have already
   paid for those lessons; don't pay twice.
2. Implement `sudoc_sdk::Backend` (see `sudoc/crates/sdk`) in a new
   `sudoc/crates/backend_<lang>` crate. The Python backend is the friendliest
   reference for dynamic targets; C for manual-memory targets; the other four
   cover most of the space between.
3. Register it — one line in `sudoc_harness::all_backends()`, one workspace
   member, one dependency. Everything else (CLI targets, lockstep, the
   conformance gate) picks it up automatically.
4. **Definition of done**: `sudoc conformance --target <yours>` green — your
   backend agrees with all existing backends on every module in
   `conformance/semantics/` — plus `cargo test --workspace` at zero failures
   (the full harness suite catches things conformance can't; we learned this
   the hard way) and zero clippy warnings.
5. Write `notes/friction-<lang>.md` as you go: every place the guide or SDK
   was unclear or wrong. Friction logs are how the guide improves — the
   existing ones are the expected caliber.

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
