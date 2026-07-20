# The external backend protocol

Version 1. This document defines how a backend written in *any* language
plugs into `sudoc`. It is the same contract as the in-process Rust
[`Backend` trait](backend-guide.md) — a type mapping, value-semantics copy
points, a trap surface, and a test runner — carried over a process boundary
as JSON. Everything in the [backend author's guide](backend-guide.md)
applies unchanged; this document only specifies the wire.

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

An external backend is a **manifest** plus an **executable**.

```jsonc
// my-backend.sudoc-backend.json
{
  "protocol": 1,
  "name": "hs",                          // CLI target name; [a-z][a-z0-9_]*
  "emit": ["runghc", "Main.hs"],         // argv; relative paths resolve
                                         //   against the manifest's directory
  "recipe": {
    "build": [["ghc", "-O", "{entry}_test.hs"]],   // may be []
    "run": ["./{entry}_test"]
  }
}
```

`sudoc test|build|conformance --external <manifest.json>` registers the
backend alongside the built-in targets for that invocation; everything
downstream (lockstep, conformance, TAP alignment) treats it identically.
`{entry}` in recipe commands is replaced with the entry module's name;
recipe commands run with the output directory as working directory, exactly
as for in-tree backends.

Per emit, `sudoc` spawns the `emit` argv **with the manifest's directory as
the working directory** — so relative arguments like `"Main.hs"` above
resolve against the backend's own files — writes one JSON request to the
child's stdin, closes it, and reads one JSON response from stdout.
**stderr is a human log** — it is passed through for diagnostics and never
parsed. Nonzero exit, malformed output, or an `error` response all surface
as a backend failure with stderr attached.

## 2. The emit request

```jsonc
{
  "protocol": 1,          // exact match required; reject anything else
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
is done when `sudoc conformance --external <manifest>` is green against the
reference backends.

## 5. What v1 deliberately leaves out

- **Discovery.** No search path, no config file; backends are named
  explicitly per invocation. A registry can come later without touching the
  wire format.
- **Streaming / long-lived servers.** One process per emit. Codegen is
  milliseconds; the simplicity is worth more than the fork saved.
- **Capability negotiation.** Exact version match instead. When the IR
  changes, external backends update — the conformance suite is the
  compatibility story, not a matrix of partial protocol support.
