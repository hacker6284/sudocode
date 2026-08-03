# Phase 5 — the `sudo_external_backend` recipe-JSON-from-attrs interface

**Task 0 decision record.** This pins the exact contract every later Phase-5
task depends on: what a `sudo_external_backend` Bazel rule must produce so that
the existing `_lockstep_test` launcher + `capture_run` run an external backend
(hs) **identically to today's `--external` manifest path**, with zero runtime
discovery and zero on-disk manifest.

Read before writing this: `capture_run.rs`, `cli/src/main.rs`
(`emit-ir`/`emit-recipe`/`build`), `backend_ext/src/lib.rs` (the emit exchange),
`tools/lockstep.bzl`, `stdlib/BUILD.bazel`, `conformance/BUILD.bazel`,
`backends/haskell/{hs.sudoc-backend.json,Emit.hs}`, `ir/src/wire.rs`,
`types/src/lib.rs` (entry-name derivation), `spec/protocol.md`.

---

## 1. What `capture_run` consumes today (the recipe contract — UNCHANGED)

`capture_run --recipe R.json --dir DIR --out OUT.json` deserializes R.json into
`sudoc_sdk::TestRecipe`:

```rust
pub struct TestRecipe {
    pub build: Vec<Vec<String>>,   // argv per build step, run in DIR, in order
    pub run:   Vec<String>,        // the run argv; its stdout is the outcome protocol
}
```

Semantics (never-fail wrapper, design §3): each `build` step runs in `DIR`; the
first failing step becomes the captured outcome (nonzero exit) and short-circuits;
otherwise `run` executes in `DIR` and `{stdout, stderr, exit_code}` is written to
`--out`. Always exits 0 for the wrapped work.

**`{entry}` is already substituted before capture_run sees it.** The recipe JSON
on disk contains the concrete stem (e.g. `arithmetic_test`), not `{entry}`.
Today that substitution happens inside `ExternalBackend::test_recipe` /
`emit-recipe`. Under the rule it happens in Starlark (§4).

**This contract does not change in Phase 5.** capture_run, TestRecipe, and the
`_lockstep_test` launcher's per-backend `codegens[i]` (a generated-source
directory) + `recipes[i]` (a recipe JSON file) integration points stay
byte-for-byte the same. Only the *source* of those two artifacts changes for the
external backend: from `sudoc build --target hs --external <manifest>` +
`sudoc emit-recipe --external <manifest>` → to the `sudo_external_backend` rule.

## 2. What the hs external path does today (end to end, to be replaced)

1. **Codegen** (`_lockstep_codegen`, `tools/lockstep.bzl`): a no-sandbox `local`
   `run_shell` runs `sudoc build --target hs --external backends/haskell/hs.sudoc-backend.json --tests -o OUT STAGE/entry.sudo`.
   Internally `sudoc build` loads the manifest, and `sudoc_backend_ext::ExternalBackend`:
   - serializes the checked program's IR via `sudoc_ir::wire::to_wire_json` (=
     `serde_json::to_string(&modules)`; the wire rules — i64-as-decimal-string,
     external enum tagging — live on the IR type serde impls),
   - wraps it in the **emit request envelope**
     `{"protocol":2,"cmd":"emit","entry":<entry-name>,"with_tests":true,"modules":<wire array>}`,
   - spawns the manifest's `emit` argv (`runghc Emit.hs`) with **cwd = the
     manifest's directory** (so `Emit.hs`'s `readFile "SudoRt.hs"` resolves) and
     the envelope on stdin,
   - reads a `{"files":[{"path","contents"},…]}` response on stdout (or
     `{"error":…}`), and writes each file into `OUT`.
   `Emit.hs` emits **multiple** files: `SudoRt.hs`, one `.hs` per module, and
   `<entry>_test.hs` (`emitAll` = `("SudoRt.hs", rt) : modFiles ++ testFiles`).

2. **Recipe** (`_lockstep_emit subcommand=emit-recipe`): a no-sandbox `local`
   `run_shell` runs `sudoc emit-recipe --target hs --external <manifest> -o R.json …`.
   `ExternalBackend::test_recipe(stem)` reads `manifest.recipe`, substitutes
   `{entry}`→stem, and writes TestRecipe JSON. For hs the manifest recipe is:
   ```json
   { "build": [["ghc","-O0","-rtsopts","-with-rtsopts=-K8m","-o","{entry}_test","{entry}_test.hs"]],
     "run":   ["./{entry}_test"] }
   ```

3. **Tests manifest** (`emit-tests`): backend-independent; unchanged.

4. **Launcher** (`_lockstep_test`): stages `codegens[i]` into a writable work
   dir and runs `capture_run --recipe recipes[i] --dir W`. hs runs under host
   `ghc`/`runghc` at test time (`tags=["local"]`, PATH inherited).

The pieces being retired (Task 3): `hs.sudoc-backend.json`, the `--external`
flag, `discovered_backends()`, `ExternalBackend` *registration via manifest*, and
the `manifest=`/`--external` attrs in `tools/lockstep.bzl`.

## 3. Entry-name fact that makes envelope construction trivial

`sudoc_types::check_program_with` derives the **entry module name from the file
stem** (`entry.file_stem()`, validated to `[A-Za-z_][A-Za-z0-9_]*`). So for entry
`arithmetic.sudo` the last module's `name` is exactly `arithmetic`. Therefore the
rule can build the emit envelope's `"entry"` field from the `entry` attr's stem
**without parsing the IR JSON** — and `Emit.hs`'s `decodeRequest` check
(`entry == last module name`) is satisfied.

`sudoc emit-ir` output is valid wire JSON: it is `serde_json::to_string_pretty(&program.modules)`,
i.e. the same data `to_wire_json` produces (pretty vs compact is irrelevant to
`Emit.hs`'s structural JSON parser). Modules come out dependency-first, entry
last — exactly the envelope's required order.

## 4. The decision — `sudo_external_backend`

### 4a. Recipe JSON is synthesized from attrs (`ctx.actions.write`)

The rule takes `recipe_build` and `recipe_run` **list-of-argv** attrs and writes
the TestRecipe JSON directly, doing the `{entry}`→stem substitution in Starlark.
No `sudoc emit-recipe`, no `--external`, no manifest read. For hs, verbatim from
the retired manifest:

```python
recipe_build = [["ghc", "-O0", "-rtsopts", "-with-rtsopts=-K8m", "-o", "{entry}_test", "{entry}_test.hs"]]
recipe_run   = ["./{entry}_test"]
```

The `-with-rtsopts=-K8m` and all ghc flags move **verbatim** from the JSON
`recipe.build` into the `recipe_build` attr (design §5: "per-backend flags move
from JSON into the respective Bazel target attributes — nothing lost"). The rule
emits `{"build": <subst(recipe_build)>, "run": <subst(recipe_run)>}` — the exact
shape §1 pins. Substitution: replace the literal token `{entry}` inside every
argv element with the entry stem.

### 4b. The emitter is an `sh_binary` speaking the UNCHANGED emit protocol

`//backends/haskell:emitter` = `sh_binary(srcs=["emit.sh"], data=["Emit.hs","SudoJson.hs","SudoRt.hs"])`
where `emit.sh` is a thin wrapper: `cd` to the runfiles dir holding the `.hs`
files (so `readFile "SudoRt.hs"` resolves) then `exec runghc Emit.hs`. It reads
**one emit-request envelope on stdin** and writes **one `{files}` response on
stdout** — the protocol `Emit.hs` already implements, untouched. (NOT
`haskell_binary`; rules_haskell stays out of scope.)

> Deviation from the plan's Task 1 sketch ("emit.sh … writes .hs to stdout"):
> `Emit.hs` emits a **multi-file `{files}` JSON response**, not a bare `.hs`
> stream, and reads the **full request envelope**, not bare IR-JSON. The sketch
> under-specified the real protocol; this record pins the real one. `Emit.hs` and
> the wire protocol are NOT modified.

### 4c. The rule's codegen action bridges emit-ir → emitter → source tree

The `sudo_external_backend` codegen is a no-sandbox `local` `run_shell`
(host `runghc`, like today), doing:

1. Stage `srcs` flat by basename into a staging dir.
2. `sudoc emit-ir -o modules.json STAGE/<entry>` → bare wire modules array
   (hermetic in-tree path; needs **no backend registry**, so it survives the
   `discovered_backends()`/`--external` deletion).
3. Build the envelope by pure string concatenation (entry = stem from the
   `entry` attr; `modules.json` is already valid JSON):
   `{"protocol":2,"cmd":"emit","entry":"<stem>","with_tests":true,"modules":<modules.json>}`
   → `request.json`.
4. Run the emitter over the protocol: `emitter < request.json > response.json`.
5. Unpack `response.json` `{files:[{path,contents}]}` into the declared output
   directory.

### 4d. Unpacking the `{files}` response — new minimal helper `emit_unpack`

Step 5 needs a robust JSON reader (multi-file; `contents` carries escaped
newlines/UTF-8). Pure shell / `jq` / `python3` in the hs codegen is fragile or
coupling. Decision: add a tiny **hermetic Rust bin
`//sudoc/crates/harness:emit_unpack`** — reads a `{files:[{path,contents}]}` JSON
on stdin, validates each path (no `..`, no absolute — mirrors
`backend_ext::validate_response_path`), writes each file under `-o DIR`. It is a
pure protocol-boundary tool: **no backend registry, no discovery** — so it is
fully compatible with deleting `discovered_backends()`/`--external`, and it keeps
the surviving IR-JSON/emit boundary (spec decision 6/§2.6) owned by first-party
hermetic code instead of a shell heuristic. Sibling to `capture_run` in shape
(small never-surprising wrapper bin).

> This is the one net-new binary Phase 5 introduces beyond the plan's literal
> text; it is the honest cost of moving the emit exchange out of the deleted
> `ExternalBackend` registration path while leaving `Emit.hs`/the wire protocol
> untouched. Flagged for the architect.

### 4e. Rule surface (what Task 1 implements)

Conceptually, per external backend the macro creates two targets plugging into
the launcher's existing `codegens[i]` / `recipes[i]` slots:

```python
sudo_external_backend(
    name,                 # unique per (module, backend)
    srcs, entry,          # same staging inputs as in-tree codegen
    emitter,              # //backends/haskell:emitter (sh_binary)
    recipe_build,         # [["ghc", …, "{entry}_test.hs"]]  — verbatim from the old manifest
    recipe_run,           # ["./{entry}_test"]
    sudoc,                # //sudoc/crates/cli:sudoc
    emit_unpack,          # //sudoc/crates/harness:emit_unpack
)
# → produces: <name> (codegen dir)  +  <name>_recipe (TestRecipe JSON)
```

Integration into `sudo_lockstep_test`: the `external` map value becomes a struct
carrying `{emitter, recipe_build, recipe_run}` (replacing the old
`[emitter_label, manifest_path]`). For an external backend the macro routes to
the `sudo_external_backend` codegen+recipe rules instead of
`_lockstep_codegen`/`_lockstep_emit`; the resulting `(dir, recipe_json)` join the
`codegens`/`recipes` lists identically to an in-tree backend. **The launcher
(`_lockstep_test`) is unchanged.**

## 5. Sequencing guardrail (why this ordering is safe)

- Task 1 adds `sudo_external_backend` + `emit_unpack` + `emit.sh` and wires a
  **parallel `hs_new`** config on one module — the old `--external hs` stays.
- Task 2 cuts every module to the rule-based hs; old path unused but present.
- Task 3 deletes `hs.sudoc-backend.json`, `--external`, `discovered_backends()`,
  `ExternalBackend` manifest registration, and the dead lockstep attrs — safe
  because nothing calls them and CI proved the new path in Task 2.

The recipe JSON schema in §1 is the fixed contract across all three.
