# Reference external backend (worked example)

A minimal, third-party-style external backend for `rules_sudo`, kept deliberately
small (spec §7 "worked example, kept minimal"). It shows the entire plugin
surface (spec §2.7 / §6):

- **`emit.sh` + `emit.py`** — an `sh_binary` *emitter* speaking the emit protocol
  (`spec/protocol.md` §2): read one emit-request envelope on stdin, write one
  `{"files":[...]}` response on stdout. This one "transpiles" a checked program to
  a tiny Python TAP script. It is a skeleton: it emits `ok` for every test (it
  does not evaluate assertions) and assumes identifier-safe test names — enough to
  lockstep-agree on the demo module and to demonstrate the wiring, not a real
  backend.
- **`sudo_external_backend(name = "pyref", emitter, recipe_build, recipe_run)`** —
  the reusable backend descriptor. A downstream author writes exactly this one
  target in their own repo.
- **`sudo_lockstep_test(backends = ["py", ":pyref"])`** — references the external
  backend BY LABEL. The test passes iff `:pyref` agrees, test-for-test, with the
  built-in `py` backend on `hello.sudo`. Adding a backend never edits the test
  macro.

## Running it

The hermetic decomposed lockstep needs the matched set of first-party binaries
(`sudoc` + `lockstep_diff` + `capture_run` + `emit_unpack`). No published release
ships them yet — they land with the matched-pair release (design §8 Phase 5 Task
8) — so this example is driven against a **HEAD build** via `sudo.local_binary`,
the same pre-release dogfood mechanism a downstream repo uses to validate before a
release:

```sh
./verify.sh
```

`verify.sh` builds the parent repo's HEAD binaries, points this module's
`sudo.local_binary(...)` at them, and runs `//:hello_lockstep_test`. After the
matched-pair release, replace the `sudo.local_binary(...)` block in `MODULE.bazel`
with `sudo.toolchain(version = "<tag>")` and the example runs against the release.
