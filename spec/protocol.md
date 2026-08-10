# The external backend protocol

Version 3. This document defines how a backend written in *any* language
plugs into `sudoc`. It is the same contract as the in-process Rust
[`Backend` trait](backend-guide.md) — a type mapping, value-semantics copy
points, a trap surface, and a test runner — carried over a process boundary
as JSON. Everything in the [backend author's guide](backend-guide.md)
applies unchanged; this document only specifies the wire.

> **v1 → v2:** record/enum-variant fields on the wire changed from 2-tuple
> arrays `[name, ty]` to named-key objects `{ "name", "ty", "boundary" }`
> so per-field `BoundaryTy` (including `text` intent) is preserved.

> **v2 → v3:** `IrParam` (function parameters; `IrTest` has none) gained a required
> `never_written` boolean field. It is a FACT about the callee body
> only: this parameter is never written (via assignment, index/field
> mutation, a mutating-builtin receiver, or forwarding as an `inout`
> argument) anywhere in the function body. It says nothing about
> call-site aliasing or whether the function's address is taken.
> Always `false` for `inout` params (consumers already gate on
> `!p.inout` first, so the field is meaningless there). Consumers that
> don't use it for codegen must still parse and validate its presence
> (strict parsing, §3).

Two design commitments, stated up front:

1. **The wire format is the single data contract.** The six in-tree
   backends are exercised through a serialize → deserialize round trip in CI
   ("wire-trip mode"), and their output must be byte-identical to the direct
   in-process path. The protocol cannot lag the IR, because the reference
   backends would fail CI the moment it did.
2. **The schema is generated, not hand-written.** The Rust IR types are the
   normative definition; `spec/protocol/ir-schema.json` is generated from
   them (schemars) and committed, with a CI drift check. A hand-written
   schema would be a second source of truth waiting to diverge.

## 1. Lifecycle

An external backend is an **emitter executable** plus a **build/run recipe**,
registered by the Bazel `sudo_external_backend` rule (`rules_sudo`):

```python
sudo_external_backend(
    name = "hs",
    emitter = ":emitter",  # any executable speaking the emit protocol (§2)
    recipe_build = [["ghc", "-O", "{entry}_test.hs"]],  # may be []
    recipe_run = ["./{entry}_test"],
)
```

The emitter is *any* executable (an `sh_binary`, a compiled tool, a script)
that speaks the emit protocol below. The recipe — the build steps and the run
command — is carried as rule attributes, not in a sidecar file; `{entry}` in a
recipe command is replaced with the entry module's name, and recipe commands
run with the output directory as working directory, exactly as for in-tree
backends. A `sudo_lockstep_test` references the backend BY LABEL in its
`backends` list, so adding a backend never edits the test rule (see §5).

Per emit, the codegen action writes one JSON request to the emitter's stdin,
closes it, and reads one JSON response from stdout. The emitter is responsible
for resolving its own support files (e.g. via its runfiles). **stderr is a
human log** — passed through for diagnostics, never parsed. Nonzero exit,
malformed output, or an `error` response all surface as a backend failure with
stderr attached.

> **Retired (Phase 5, spec §2.4/§2.6):** the pre-Bazel registration surface —
> the `*.sudoc-backend.json` manifest, `sudoc build/emit-recipe --external
> <manifest>`, and filesystem auto-discovery of manifests — is gone. Its emit
> exchange (§2–§4) is the **surviving contract**, now driven by the Bazel build
> graph instead of runtime discovery. The manifest's `emit` argv became the
> `emitter` executable; its `recipe` became the `recipe_build`/`recipe_run`
> attrs, verbatim.

## 2. The emit request

```jsonc
{
  "protocol": 3,          // exact match required; reject anything else
  "cmd": "emit",
  "entry": "sorting",     // entry module name (last element of modules)
  "with_tests": true,     // if true: entry's tests must become a runnable
                          //   artifact speaking the TAP-ish outcome protocol
  "modules": [ /* IrModule, dependency order, entry last */ ]
}
```

The response:

```jsonc
{ "files": [ { "path": "sorting.hs", "contents": "..." }, ... ] }
```

or `{ "error": "human-readable message" }`. `files` must include everything
the recipe needs — generated modules *and* any runtime support files
(the in-tree `runtime_files()` distinction is collapsed; external backends
simply return the full set). Paths are relative to the output directory,
no leading separators, no `..`.

## 3. IR encoding

`spec/protocol/ir-schema.json` is normative for structure. Rules that a
schema cannot express:

- **Versioning is exact-match.** Any change to the IR's wire shape bumps
  `protocol`, and consumers must reject unknown versions and unknown
  fields/variants (parse strictly — a backend that ignores a statement kind
  it doesn't recognize would emit silently wrong code).
- **`int` values are decimal strings**, not JSON numbers: `"‑42"` ranges
  over the full i64 domain, and JSON parsers in f64-based languages corrupt
  integers beyond 2^53. This covers `Int` literals, `Int` match patterns,
  and every other i64 leaf — **except** text scalar values (below).
- **Text literals** remain arrays of plain JSON numbers (Unicode scalar
  values, ≤ 0x10FFFF — comfortably exact in every parser).
- **Floats**: finite values are JSON numbers in shortest round-trip decimal
  form (both serde_json and IEEE-754-native consumers reproduce the exact
  bit pattern). Non-finite values are the strings `"nan"`, `"inf"`,
  `"-inf"`; they cannot appear in v1 IR (no literal or foldable expression
  produces them) but the encoding is reserved so wire-trip can never flake.
- **Enums use serde's external tagging**: unit variants are bare strings
  (`"Skip"`, `"Break"`), payload variants are single-key objects
  (`{"Int": "42"}`, `{"While": {"cond": ..., "body": [...]}}`).
- **`Ty::Infer` never crosses the wire.** It is a checker-internal variable;
  serialization of a module containing one is a `sudoc` bug and errors out.
- **Boundary types are closed.** Export signatures carry a `BoundaryTy` —
  structurally the resolved type with `text` preserved — never the surface
  AST. (The syntax crate's types are not part of this contract.)

## 4. Obligations on the emitted code

Identical to every backend's (guide §2): the test artifact prints
`ok N - name` / `not ok N - name [TrapKind: detail]` lines in declaration
order using `test_fn_names` naming, exits nonzero iff any test failed, and
the generated library code upholds sudo semantics — value semantics, the
trap surface, unspecified Map/Set order — as pinned by
`conformance/semantics/`. **Acceptance is unchanged**: an external backend
is done when the corpus lockstep is green against the reference backends —
`bazel test //conformance/...` with the backend registered as a
`sudo_external_backend` target and added to the lockstep `backends` lists
(exactly as the `hs` backend is; see `backends/haskell/BUILD.bazel` and the
worked example in `rules_sudo/examples/reference_backend/`).

## 5. Registration

External backends are registered by the Bazel build graph, not by runtime
filesystem discovery. Each is a `sudo_external_backend` target (§1); a
`sudo_lockstep_test` fans out over its `backends` list, where an entry is
either a built-in language name (`"py"`, `"c"`, …) or a LABEL to a
`sudo_external_backend` target (`"//backends/haskell:hs"`). Adding a backend is
a one-target edit that never touches `sudo_lockstep_test`. A downstream plugin
author writes exactly one `sudo_external_backend` in their own repo and
references it by label — the whole plugin surface (see
`rules_sudo/examples/reference_backend/`).

Independent implementations of an already-covered language are welcome —
register a distinct target (`myzig` beside `zig`) and add both to a lockstep
`backends` list, which then diffs the two implementations against each other.

> **Retired (Phase 5):** auto-discovery of `backends/*/*.sudoc-backend.json`
> and the `--external` escape hatch. Registration is now explicit in BUILD
> files; the build graph is the single source of which backends exist.

## 6. Hosting policy — two front doors, one gate

In-tree (Rust trait) and external (this protocol) are equal ways to be a
sudo backend. The choice is per-emitter engineering — made by whoever
maintains the backend, on criteria like compiler-language fit, platform
coverage, toolchain stability, and maintainer fluency (see the
[backend guide §0](backend-guide.md)) — and a backend may migrate between
hostings without its target's standing changing. What is uniform, and
enforced mechanically rather than by promise: the data contract (wire-trip
CI proves in-tree backends see exactly what the wire carries), the
acceptance bar (conformance against the reference backends), and the gate
(every backend in this repo, either hosting, must be green on every push —
an IR change lands with all backends updated in the same change, external
ones included).

## 7. What v1 deliberately leaves out

- **Streaming / long-lived servers.** One process per emit. Codegen is
  milliseconds; the simplicity is worth more than the fork saved.
- **Capability negotiation.** Exact version match instead. When the IR
  changes, external backends update — the conformance suite is the
  compatibility story, not a matrix of partial protocol support.
