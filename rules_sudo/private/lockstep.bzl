"""Decomposed hermetic-where-feasible lockstep, packaged as rules_sudo 1.0.0.

Moved verbatim-in-spirit from the root repo's `//tools:lockstep.bzl` (Bazel
migration Phase 1a/1b) into the public ruleset (Phase 5, spec §4/§6). Two
public entry points, re-exported from `//:defs.bzl`:

  * `sudo_lockstep_test(name, lib, entry, backends)` — LIB-BASED (a
    `sudo_library` target + a backend list), replacing the PATH-inheriting
    v0.2.1 test (breaking → 1.0.0, compatibility_level 2, spec §2.5). Per
    backend the DAG is `lockstep_diff(tests-manifest, N captured-run files)`
    (design §3):
      - codegen (per backend)  — in-tree: `sudoc build --target L --tests`;
                                 external: emit-ir → emit protocol → emit_unpack.
      - recipe  (per backend)  — in-tree: `sudoc emit-recipe --target L`;
                                 external: `{build, run}` written from the
                                 backend's `recipe_build`/`recipe_run` attrs.
      - tests-manifest         — `sudoc emit-tests` (backend-independent).
      - run leaf (per backend) — `capture_run --recipe R --dir W` (never-fail).
      - lockstep_diff          — diffs per-test outcomes; nonzero on divergence.

  * `sudo_external_backend(name, emitter, recipe_build, recipe_run)` — a
    STANDALONE backend descriptor (spec §6/§2.7): one target, produced once by
    a plugin author in their own repo, carrying a `SudoBackendInfo` provider.
    `sudo_lockstep_test` consumes it BY LABEL in `backends` — adding a backend
    never edits `sudo_lockstep_test`. Replaces the retired runtime-discovery
    manifest / `--external` path with recipe-JSON-from-attrs (Phase 5 Task 0
    decision record, notes/phase5-external-backend-interface.md).

Backend targets in the `backends` list are either bare in-tree language names
(`"py"`, `"js"`, `"c"`, `"rs"`, `"zig"`, `"swift"`) or LABELS to
`sudo_external_backend` targets (e.g. `"//backends/haskell:hs"`).

Scope note (unchanged from the root-repo original): codegen, recipe and the
tests-manifest are hermetic cached build actions; the **run leaves execute at
test time** (`tags=["local"]`, PATH inherited). py/rs use a pinned
rules_python/rules_rust toolchain from runfiles (design §5/§9 "verifiable
slice"); c/js/zig/swift/hs use host toolchains. Turning the remaining run
leaves hermetic is out of Phase-5 scope (design §9).

The `sudoc`/`capture_run`/`lockstep_diff`/`emit_unpack` label attrs default to
`@sudo_toolchain//:*` (the release toolchain the module extension provides). A
consumer building the tools from source (like sudocode dogfooding itself)
overrides them to its own first-party targets.
"""

load(":rules.bzl", "SudoInfo")

# ---------------------------------------------------------------------------
# Provider: a backend descriptor. A *backend* is anything that, given generated
# source for a module, produces that module's captured-run file (spec §6). An
# external backend expresses that as an emitter (IR-JSON → source over the emit
# protocol) plus a compile/run recipe.
# ---------------------------------------------------------------------------

SudoBackendInfo = provider(
    doc = "An external sudo backend: an emitter executable + a compile/run recipe.",
    fields = {
        "emitter": "FilesToRunProvider — the emitter (reads an emit-request envelope on stdin, writes a {files} response on stdout).",
        "recipe_build": "list of argv lists — the build steps ({entry} is the entry stem placeholder).",
        "recipe_run": "argv list — the run command ({entry} placeholder).",
    },
)

_TOOLCHAIN_DEFAULTS = {
    "sudoc": "@sudo_toolchain//:sudoc",
    "capture_run": "@sudo_toolchain//:capture_run",
    "lockstep_diff": "@sudo_toolchain//:lockstep_diff",
    "emit_unpack": "@sudo_toolchain//:emit_unpack",
}

def _entry_stem(entry):
    base = entry.split("/")[-1]
    if base.endswith(".sudo"):
        return base[:-len(".sudo")]
    return base

def _stage_command(srcs, stage):
    """Shell lines copying each .sudo src by basename into a flat staging dir
    (sudoc resolves file imports relative to the importing file's dir)."""
    lines = ['mkdir -p "%s"' % stage]
    for f in srcs:
        lines.append('cp "{src}" "{stage}/$(basename "{src}")"'.format(src = f.path, stage = stage))
    return "\n".join(lines)

def _lib_srcs(ctx):
    return ctx.attr.lib[SudoInfo].transitive_srcs.to_list()

# ---------------------------------------------------------------------------
# In-tree codegen + emit (sudoc build / emit-recipe / emit-tests).
# ---------------------------------------------------------------------------

def _codegen_impl(ctx):
    """In-tree backend codegen: `sudoc build --target LANG --tests`. External
    backends go through `sudo_external_backend`, not this rule."""
    sudoc = ctx.executable.sudoc
    src_list = _lib_srcs(ctx)
    out_dir = ctx.actions.declare_directory(ctx.label.name)
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]

    command = """
set -euo pipefail
{stage}
"{sudoc}" build --target {lang} --tests -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(src_list, stage),
        sudoc = sudoc.path,
        lang = ctx.attr.lang,
        out = out_dir.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = src_list,
        outputs = [out_dir],
        tools = [sudoc],
        command = command,
        mnemonic = "SudoCodegen",
        progress_message = "sudoc codegen %{label} (--target " + ctx.attr.lang + ")",
    )
    return [DefaultInfo(files = depset([out_dir]), runfiles = ctx.runfiles(files = [out_dir]))]

_lockstep_codegen = rule(
    implementation = _codegen_impl,
    attrs = {
        "lib": attr.label(providers = [SudoInfo], mandatory = True),
        "entry": attr.string(mandatory = True),
        "lang": attr.string(mandatory = True),
        "sudoc": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
    },
)

def _sudoc_emit_impl(ctx):
    """Stage srcs, run a sudoc emit-* subcommand writing a single JSON file
    (emit-tests: the tests manifest; emit-recipe: an in-tree backend's recipe)."""
    sudoc = ctx.executable.sudoc
    src_list = _lib_srcs(ctx)
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]

    # The C recipe's sanitizer flags come from a `cc -fsanitize=...` support
    # probe (backend_c). That probe MUST see the same host cc the run-leaf will
    # compile with — inside the hermetic emit sandbox it fails (no host toolchain
    # / restricted fs) and silently yields an UNINSTRUMENTED recipe, disabling
    # the C sanitizer gate. So emit-recipe runs as a no-sandbox `local` action
    # with the host shell env (like the run-leaf). emit-tests is pure and stays
    # hermetic + cacheable.
    is_recipe = ctx.attr.subcommand == "emit-recipe"

    command = """
set -euo pipefail
{stage}
"{sudoc}" {subcmd} {targ} -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(src_list, stage),
        sudoc = sudoc.path,
        subcmd = ctx.attr.subcommand,
        targ = "--target " + ctx.attr.lang if ctx.attr.lang else "",
        out = out.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = src_list,
        outputs = [out],
        tools = [sudoc],
        command = command,
        mnemonic = "SudoEmit",
        progress_message = "sudoc " + ctx.attr.subcommand + " %{label}",
        use_default_shell_env = is_recipe,
        execution_requirements = {"local": "1"} if is_recipe else {},
    )
    return [DefaultInfo(files = depset([out]))]

_lockstep_emit = rule(
    implementation = _sudoc_emit_impl,
    attrs = {
        "lib": attr.label(providers = [SudoInfo], mandatory = True),
        "entry": attr.string(mandatory = True),
        "subcommand": attr.string(mandatory = True),  # "emit-tests" | "emit-recipe"
        "lang": attr.string(default = ""),  # backend for emit-recipe; empty for emit-tests
        "sudoc": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
    },
)

def _protocol_stamp_impl(ctx):
    """Capture `sudoc protocol-version` into a file, so the test launcher can
    run-time-handshake it against `lockstep_diff --protocol-version` (a
    mismatched sudoc/lockstep_diff pair fails loudly instead of misdiffing).
    sudoc runs at build time; the stamp travels to the test as a runfile."""
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.run_shell(
        outputs = [out],
        tools = [ctx.executable.sudoc],
        command = '"%s" protocol-version > "%s"' % (ctx.executable.sudoc.path, out.path),
        mnemonic = "SudoProtocolStamp",
        progress_message = "sudoc protocol-version %{label}",
    )
    return [DefaultInfo(files = depset([out]))]

_protocol_stamp = rule(
    implementation = _protocol_stamp_impl,
    attrs = {
        "sudoc": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
    },
)

# ---------------------------------------------------------------------------
# External backend: descriptor rule + codegen/recipe consumers.
#
# `sudo_external_backend` (macro → `_sudo_external_backend` rule) produces a
# SudoBackendInfo target. Per (module, backend) the `sudo_lockstep_test` macro
# instantiates `_external_codegen` (emit-ir → envelope → emitter → emit_unpack →
# source dir) and `_external_recipe` (recipe JSON from the descriptor's attrs),
# both reading the descriptor BY LABEL. The launcher consumes the resulting
# (dir, recipe.json) pair identically to an in-tree backend — it is unchanged.
# ---------------------------------------------------------------------------

def _sudo_external_backend_impl(ctx):
    return [SudoBackendInfo(
        emitter = ctx.attr.emitter[DefaultInfo].files_to_run,
        recipe_build = json.decode(ctx.attr.recipe_build),
        recipe_run = json.decode(ctx.attr.recipe_run),
    )]

_sudo_external_backend = rule(
    implementation = _sudo_external_backend_impl,
    doc = "A standalone external-backend descriptor (emitter + compile/run recipe).",
    attrs = {
        "emitter": attr.label(
            executable = True,
            cfg = "exec",
            mandatory = True,
            doc = "Executable target speaking the emit protocol (IR-JSON envelope on stdin → {files} response on stdout).",
        ),
        # list-of-list / list are not expressible as rule attrs, so the macro
        # json-encodes them and the rule decodes into the provider.
        "recipe_build": attr.string(mandatory = True),
        "recipe_run": attr.string(mandatory = True),
    },
)

def sudo_external_backend(name, emitter, recipe_build, recipe_run, **kwargs):
    """Register an external backend as a reusable, referenceable target.

    A downstream plugin author writes ONE of these in their own repo and
    references it by label in `sudo_lockstep_test(backends = [..., ":my_backend"])`
    (spec §2.7). Produces a `SudoBackendInfo` target.

    Args:
      name: target name.
      emitter: an executable target that reads an emit-request envelope on
        stdin and writes a `{"files":[{"path","contents"}]}` response on stdout
        (the unchanged emit protocol, spec/protocol.md §2).
      recipe_build: list of argv lists — the backend's build steps. The literal
        token `{entry}` in any element is substituted with the entry stem.
      recipe_run: argv list — the backend's run command (`{entry}` substituted).
    """
    _sudo_external_backend(
        name = name,
        emitter = emitter,
        recipe_build = json.encode(recipe_build),
        recipe_run = json.encode(recipe_run),
        **kwargs
    )

def _external_codegen_impl(ctx):
    sudoc = ctx.executable.sudoc
    emit_unpack = ctx.executable.emit_unpack
    emitter = ctx.attr.backend[SudoBackendInfo].emitter
    src_list = _lib_srcs(ctx)
    out_dir = ctx.actions.declare_directory(ctx.label.name)
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]
    stem = _entry_stem(ctx.attr.entry)

    # Build the emit-request envelope by pure concatenation: emit-ir output is
    # already valid wire JSON (modules array, entry last) and the entry module
    # name equals the file stem (types::check_program), so no JSON parsing is
    # needed. printf's format string carries literal JSON braces, so keep it out
    # of str.format() by concatenating the command.
    command = "\n".join([
        "set -euo pipefail",
        _stage_command(src_list, stage),
        '"%s" emit-ir -o "%s/modules.json" "%s/%s"' % (sudoc.path, stage, stage, entry),
        'printf \'{"protocol":3,"cmd":"emit","entry":"%s","with_tests":true,"modules":\' > "%s/request.json"' % (stem, stage),
        'cat "%s/modules.json" >> "%s/request.json"' % (stage, stage),
        'printf \'}\' >> "%s/request.json"' % stage,
        '"%s" < "%s/request.json" > "%s/response.json"' % (emitter.executable.path, stage, stage),
        '"%s" -o "%s" < "%s/response.json"' % (emit_unpack.path, out_dir.path, stage),
    ]) + "\n"

    ctx.actions.run_shell(
        inputs = src_list,
        outputs = [out_dir],
        # `emitter` is a FilesToRunProvider from the backend descriptor; passing
        # it in `tools` materializes its runfiles (Emit.hs / SudoRt.hs / …).
        tools = [sudoc, emit_unpack, emitter],
        command = command,
        mnemonic = "SudoExternalCodegen",
        progress_message = "sudoc external codegen %{label}",
        # The emitter runs a host interpreter (e.g. `runghc`) — not a Bazel
        # toolchain — so this is a no-sandbox `local` action inheriting host
        # PATH, exactly like the retired `--external` hs codegen leaf.
        use_default_shell_env = True,
        execution_requirements = {"local": "1"},
    )
    return [DefaultInfo(files = depset([out_dir]), runfiles = ctx.runfiles(files = [out_dir]))]

_external_codegen = rule(
    implementation = _external_codegen_impl,
    attrs = {
        "lib": attr.label(providers = [SudoInfo], mandatory = True),
        "entry": attr.string(mandatory = True),
        "backend": attr.label(providers = [SudoBackendInfo], cfg = "exec", mandatory = True),
        "sudoc": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
        "emit_unpack": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
    },
)

def _external_recipe_impl(ctx):
    binfo = ctx.attr.backend[SudoBackendInfo]
    stem = _entry_stem(ctx.attr.entry)
    build_subst = [[a.replace("{entry}", stem) for a in step] for step in binfo.recipe_build]
    run_subst = [a.replace("{entry}", stem) for a in binfo.recipe_run]
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    ctx.actions.write(output = out, content = json.encode({"build": build_subst, "run": run_subst}))
    return [DefaultInfo(files = depset([out]))]

_external_recipe = rule(
    implementation = _external_recipe_impl,
    attrs = {
        "backend": attr.label(providers = [SudoBackendInfo], mandatory = True),
        "entry": attr.string(mandatory = True),
    },
)

# ---------------------------------------------------------------------------
# The test launcher (unchanged in shape from the root-repo original).
# ---------------------------------------------------------------------------

def _rf_path(f):
    """Map File.short_path to (kind, path-under-root) for runfiles resolution."""
    sp = f.short_path
    if sp.startswith("../"):
        return ("external", sp[3:])
    return ("workspace", sp)

# Runfiles-resolution bash helper.
_RF_PRELUDE = r"""
if [[ -n "${TEST_SRCDIR:-}" ]]; then
  RUNFILES="${TEST_SRCDIR}"
elif [[ -d "$(dirname "$0").runfiles" ]]; then
  RUNFILES="$(cd "$(dirname "$0").runfiles" && pwd)"
else
  echo "sudo_lockstep_test: cannot locate runfiles" >&2; exit 1
fi
WS="${TEST_WORKSPACE:-_main}"
rf() {
  local kind="$1" rel="$2"
  if [[ "$kind" == "external" ]]; then
    echo "${RUNFILES}/${rel}"
  elif [[ -e "${RUNFILES}/${WS}/${rel}" ]]; then
    echo "${RUNFILES}/${WS}/${rel}"
  else
    echo "${RUNFILES}/${rel}"
  fi
}
"""

def _test_impl(ctx):
    entry_stem = _entry_stem(ctx.attr.entry)
    capture_kind, capture_rel = _rf_path(ctx.executable.capture_run)
    diff_kind, diff_rel = _rf_path(ctx.executable.lockstep_diff)
    tests_kind, tests_rel = _rf_path(ctx.file.tests)
    stamp_kind, stamp_rel = _rf_path(ctx.file.protocol_stamp)

    lines = [
        "#!/usr/bin/env bash",
        "set -uo pipefail",
        "# Generated by sudo_lockstep_test — do not edit.",
        _RF_PRELUDE,
        'CAPTURE="$(rf "%s" "%s")"' % (capture_kind, capture_rel),
        'DIFF="$(rf "%s" "%s")"' % (diff_kind, diff_rel),
        'TESTS="$(rf "%s" "%s")"' % (tests_kind, tests_rel),
        'OUT="${TEST_TMPDIR:-/tmp}"',
        # Run-time matched-pair handshake (spec §2.5 / Phase 4.5): the sudoc that
        # built the artifacts and the lockstep_diff consuming them must speak the
        # same wire protocol. Fail loudly on a mismatched pair rather than emit a
        # wrong diff. Inert in this dogfood build (both come from one commit);
        # the guard is for a downstream release toolchain.
        'SUDOC_PROTO="$(cat "$(rf "%s" "%s")")"' % (stamp_kind, stamp_rel),
        'DIFF_PROTO="$("$DIFF" --protocol-version)"',
        'if [[ "$SUDOC_PROTO" != "$DIFF_PROTO" ]]; then',
        '  echo "sudo_lockstep_test: PROTOCOL MISMATCH — sudoc emitted artifacts at protocol $SUDOC_PROTO but lockstep_diff speaks $DIFF_PROTO (mismatched sudoc/lockstep_diff pair)" >&2',
        "  exit 1",
        "fi",
        # The run leaves inherit only PATH (tags=local). Give zig writable
        # caches: LOCAL under the per-test OUT; GLOBAL at a stable shared path
        # (default $HOME/.cache/sudo-zig, overridable via SUDO_ZIG_GLOBAL_CACHE_DIR).
        # Do NOT override HOME: on CI rustc is a rustup shim that resolves its
        # toolchain via ~/.rustup, so a clobbered HOME breaks the rs backend.
        # swiftc uses TMPDIR for its implicit module cache, so it needs nothing
        # extra.
        # GLOBAL is content-addressed (std-lib compile etc.) and safe to share
        # across tests and bazel runs; LOCAL holds per-build artifacts, so
        # sharing it risks cross-test interference. Zig's global cache uses
        # file locking for concurrent multi-shard access (tags=["local"]).
        'export ZIG_GLOBAL_CACHE_DIR="${SUDO_ZIG_GLOBAL_CACHE_DIR:-${HOME:-${TMPDIR:-/tmp}}/.cache/sudo-zig}"',
        'mkdir -p "$ZIG_GLOBAL_CACHE_DIR"',
        'export ZIG_LOCAL_CACHE_DIR="$OUT/.zig-local-cache"',
        "DIFF_ARGS=()",
    ]

    backends = ctx.attr.backends
    extra_runfiles = []

    # Hermetic py/rs run-leaves (design §5/§9 "verifiable slice"): the py and rs
    # backends compile/run with a pinned toolchain provided as runfiles instead
    # of whatever's on the runner's PATH. Each is exposed by prepending the real
    # toolchain `bin/` dir to PATH for *that backend's* capture_run only (so the
    # other backends stay host). We prepend the actual bin dir rather than
    # symlinking the binary because both CPython (python-build-standalone) and
    # rustc resolve their home/sysroot relative to the real executable path.
    # c/js/zig/swift/hs remain host toolchains — hermetic_cc can't link the C
    # sanitizers (spike, §5) and rules_zig/rules_swift/rules_haskell aren't
    # fetchable in every build env; those move in a later hermetic-run-leaf pass.
    if "py" in backends:
        py3 = ctx.toolchains["@rules_python//python:toolchain_type"].py3_runtime
        py_kind, py_rel = _rf_path(py3.interpreter)
        extra_runfiles.append(py3.files)
        lines += [
            "# hermetic python interpreter (py backend)",
            'PY_BIN="$(dirname "$(rf "%s" "%s")")"' % (py_kind, py_rel),
        ]
    if "rs" in backends:
        rs_tc = ctx.toolchains["@rules_rust//rust:toolchain_type"]
        rs_kind, rs_rel = _rf_path(rs_tc.rustc)
        extra_runfiles.append(rs_tc.all_files)
        lines += [
            "# hermetic rustc + its dylib dir (rs backend)",
            'RUSTC_RF="$(rf "%s" "%s")"' % (rs_kind, rs_rel),
            'RS_BIN="$(dirname "$RUSTC_RF")"',
            'RS_LIB="$(cd "$RS_BIN/../lib" && pwd)"',
        ]

    # Per-backend command prefix that injects the hermetic toolchain (empty for
    # host backends).
    # rs: rustc finds its own sysroot from the real binary path, but needs its
    # dylib dir on the loader path — LD_LIBRARY_PATH on Linux, DYLD_LIBRARY_PATH
    # on macOS (each is a harmless no-op on the other OS, so set both).
    prefix = {
        "py": 'PATH="$PY_BIN:$PATH" ',
        "rs": 'PATH="$RS_BIN:$PATH" ' +
              'LD_LIBRARY_PATH="$RS_LIB${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ' +
              'DYLD_LIBRARY_PATH="$RS_LIB${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" ',
    }

    # codegens[i] and recipes[i] are parallel to backends[i].
    for i in range(len(backends)):
        backend = backends[i]
        gen_kind, gen_rel = _rf_path(ctx.files.codegens[i])
        rec_kind, rec_rel = _rf_path(ctx.files.recipes[i])
        lines += [
            '# --- backend %s ---' % backend,
            'GEN="$(rf "%s" "%s")"' % (gen_kind, gen_rel),
            'RECIPE="$(rf "%s" "%s")"' % (rec_kind, rec_rel),
            'W="$OUT/%s_work"' % backend,
            'rm -rf "$W"; cp -r "$GEN" "$W"; chmod -R u+w "$W"',
            '%s"$CAPTURE" --recipe "$RECIPE" --dir "$W" --out "$OUT/%s.json"' %
            (prefix.get(backend, ""), backend),
            'DIFF_ARGS+=(--run %s="$OUT/%s.json")' % (backend, backend),
        ]

    lines.append('exec "$DIFF" --module "%s" --tests "$TESTS" "${DIFF_ARGS[@]}"' % entry_stem)

    launcher = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(output = launcher, content = "\n".join(lines) + "\n", is_executable = True)

    runfiles = ctx.runfiles(
        files = ctx.files.codegens + ctx.files.recipes +
                [ctx.file.tests, ctx.file.protocol_stamp, ctx.executable.capture_run, ctx.executable.lockstep_diff],
        transitive_files = depset(transitive = extra_runfiles) if extra_runfiles else None,
    )
    runfiles = runfiles.merge(ctx.attr.capture_run[DefaultInfo].default_runfiles)
    runfiles = runfiles.merge(ctx.attr.lockstep_diff[DefaultInfo].default_runfiles)
    return [
        DefaultInfo(executable = launcher, runfiles = runfiles),
        RunEnvironmentInfo(inherited_environment = ["PATH"]),
    ]

_lockstep_test = rule(
    implementation = _test_impl,
    test = True,
    attrs = {
        "entry": attr.string(mandatory = True),
        "backends": attr.string_list(mandatory = True),
        "codegens": attr.label_list(allow_files = True, mandatory = True),
        "recipes": attr.label_list(allow_files = True, mandatory = True),
        "tests": attr.label(allow_single_file = True, mandatory = True),
        "protocol_stamp": attr.label(allow_single_file = True, mandatory = True),
        "capture_run": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
        "lockstep_diff": attr.label(executable = True, cfg = "exec", allow_single_file = True, mandatory = True),
    },
    # The py/rs run-leaves use a pinned interpreter/rustc from these toolchains
    # (resolved for the test's target platform, hermetic on Linux and macOS).
    toolchains = [
        "@rules_python//python:toolchain_type",
        "@rules_rust//rust:toolchain_type",
    ],
)

def _is_backend_label(b):
    """A backend list entry is an external-backend LABEL (vs. a bare in-tree
    language name) iff it looks like a label."""
    return ":" in b or "/" in b or b.startswith("@")

def _backend_short_name(label):
    if ":" in label:
        return label.split(":")[-1]
    return label.split("/")[-1]

def sudo_lockstep_test(
        name,
        lib,
        entry,
        backends = ["py", "js"],
        sudoc = _TOOLCHAIN_DEFAULTS["sudoc"],
        capture_run = _TOOLCHAIN_DEFAULTS["capture_run"],
        lockstep_diff = _TOOLCHAIN_DEFAULTS["lockstep_diff"],
        emit_unpack = _TOOLCHAIN_DEFAULTS["emit_unpack"],
        timeout = "long",
        tags = [],
        **kwargs):
    """Lockstep-test a `sudo_library` across `backends` via the decomposed DAG.

    Args:
      name: test target name.
      lib: a `sudo_library` target (this module + its transitive file-import
        deps). Its transitive sources are staged and compiled per backend.
      entry: basename of the entry `.sudo` file within `lib` (e.g. "gcd.sudo").
      backends: a list mixing bare in-tree language names (`"py"`, `"js"`,
        `"c"`, `"rs"`, `"zig"`, `"swift"`) and LABELS to `sudo_external_backend`
        targets (e.g. `"//backends/haskell:hs"`).
      sudoc/capture_run/lockstep_diff/emit_unpack: toolchain binary label
        overrides (default `@sudo_toolchain//:*`).
      timeout: test timeout (default "long" — a multi-target suite runs minutes,
        and Bazel's default 300s "moderate" is exactly the wrong size).
      tags: extra test tags ("local" is always added; the run leaves shell out
        to host toolchains).
    """
    codegens = []
    recipes = []
    backend_names = []
    for b in backends:
        if _is_backend_label(b):
            bn = _backend_short_name(b)
            gen = "{}_{}_gen".format(name, bn)
            rec = "{}_{}_recipe".format(name, bn)
            _external_codegen(
                name = gen,
                lib = lib,
                entry = entry,
                backend = b,
                sudoc = sudoc,
                emit_unpack = emit_unpack,
            )
            _external_recipe(
                name = rec,
                backend = b,
                entry = entry,
            )
            backend_names.append(bn)
        else:
            gen = "{}_{}_gen".format(name, b)
            rec = "{}_{}_recipe".format(name, b)
            _lockstep_codegen(
                name = gen,
                lib = lib,
                entry = entry,
                lang = b,
                sudoc = sudoc,
            )
            _lockstep_emit(
                name = rec,
                lib = lib,
                entry = entry,
                subcommand = "emit-recipe",
                lang = b,
                sudoc = sudoc,
            )
            backend_names.append(b)
        codegens.append(":" + gen)
        recipes.append(":" + rec)

    _lockstep_emit(
        name = name + "_tests",
        lib = lib,
        entry = entry,
        subcommand = "emit-tests",
        sudoc = sudoc,
    )

    _protocol_stamp(
        name = name + "_protocol",
        sudoc = sudoc,
    )

    test_tags = list(tags)
    if "local" not in test_tags:
        test_tags.append("local")
    _lockstep_test(
        name = name,
        entry = entry,
        backends = backend_names,
        codegens = codegens,
        recipes = recipes,
        tests = ":" + name + "_tests",
        protocol_stamp = ":" + name + "_protocol",
        capture_run = capture_run,
        lockstep_diff = lockstep_diff,
        timeout = timeout,
        tags = test_tags,
        **kwargs
    )
