# Changelog

## v0.4.0

- **BREAKING — `record`/`enum` names may no longer contain `_`.** Type names are the only bare user data inside generated type symbols, where `_` is the component separator; an underscore made distinct types collide onto one symbol (`record List_3i64` vs `List<int>`; `Map<a_b,c>` vs `Map<a,b_c>`), which backends emitted twice as invalid code. `sudoc check` now rejects such names at the declaration site with a CamelCase rename suggestion. No deprecation cycle. Migration: rename the type (the diagnostic tells you to what). Zero impact on the stdlib, examples, conformance corpus, or any known consumer — no underscore-bearing type name existed in any of them.
- Check-time guard: mangled-symbol uniqueness is enforced post-monomorphization (`types/mangle_check`) so colliding type symbols fail `sudoc check` loudly, naming both types. Retained permanently as an oracle against future mangle-grammar changes (unit tests build colliding `Ty` values directly now that no valid source can reach them).
- Backend hygiene: dead `ListSort` arm in `backend_rs` is now `unreachable!()` naming the invariant; `backend_zig` renames `unsupported()` → `unreachable_shape()` with internal-error framing (every call site is checker-guarded; "not yet implemented" misread as open gaps).
- Docs/notes: close stale footgun claims from a repo-wide sweep (retired global-arena model, decision-log backlog items that shipped, dangling cross-references). No runtime/API impact.

## v0.3.0

See https://github.com/hacker6284/sudocode/releases/tag/v0.3.0

## v0.2.0

See https://github.com/hacker6284/sudocode/releases/tag/v0.2.0

## v0.1.0

See https://github.com/hacker6284/sudocode/releases/tag/v0.1.0
