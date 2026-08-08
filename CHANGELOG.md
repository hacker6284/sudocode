# Changelog

## v0.5.0

- **Haskell `List<T>` is now `Data.Sequence`.** The backend previously represented lists as plain `[a]`, so every indexed op was O(n) and sequential append was quadratic. Now O(1) append and O(log n) index/update. Measured: 20k appends 2.70s → 0.61s; 40k appends 10.61s → 0.71s (was quadratic — quadruples when N doubles — now flat); 1M appends previously StackOverflowed, now completes in 2.35s (python: 1.00s for contrast); 100k indexed ops 2.39s → 0.69s. Generated Haskell changes shape but not observable behavior: assert-failure diagnostics byte-identical to python; all 41 lockstep targets across 7 backends plus the trap-strictness regression pass.
- `sudoc check` accumulates signature-pass errors instead of reporting only the first (records/enums/signatures/consts, passes 1–4). Hard barrier between passes so no pass runs on a poisoned context; a file with N independent signature errors reports all N. Single-error behavior and existing error text unchanged.
- New `bench/` directory: manual performance harness (`list_append.sudo`, `list_indexed.sudo`, `run.sh`) used for the F11 numbers above. Deliberately not wired into CI/bazel (action caching would confound timing).
- CI: GitHub Actions bumped off the deprecated Node 20 runtime (checkout v4→v7, upload-artifact v4→v7, download-artifact v4→v8, cache v4→v6, action-gh-release v2→v3, setup-bazel 0.15.0→0.19.0).
- Notes/docs: F2/F11 attribution and scope corrections (internal only).

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
