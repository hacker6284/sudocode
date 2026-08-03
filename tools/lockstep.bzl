"""Decomposed lockstep conformance rules (Bazel migration Phase 1a/1b).

Per `.sudo` module the lockstep DAG is `lockstep_diff(tests-manifest,
N captured-run files)` (design §3):

  * codegen (per backend)  — `sudoc build --target L --tests` → generated source
                             tree. A hermetic, cached build action.
  * recipe  (per backend)  — `sudoc emit-recipe --target L` → the backend's own
                             build+run TestRecipe as JSON. Cached. This is the
                             single source of truth for per-backend compile flags
                             / sanitizers / libm — Bazel never re-encodes them.
  * tests-manifest         — `sudoc emit-tests` → JSON test-name array. Cached.
                             Without it a test absent from every backend vanishes.
  * run leaf (per backend) — `capture_run --recipe R --dir W` runs the recipe's
                             build steps then its run command, capturing
                             {stdout, stderr, exit_code} (never-fail: always
                             exits 0) so a crash still yields a captured file.
  * lockstep_diff          — reads the manifest + N captured files, diffs the
                             per-test outcomes, prints the divergence table, exits
                             nonzero on any divergence/failure.

Scope note: codegen, recipe and the tests-manifest are hermetic cached build
actions. The **run leaves execute at test time** (`tags=["local"]`, PATH
inherited). Their toolchains are partly hermetic:

  * **py, rs** use a pinned interpreter / rustc from `rules_python` /
    `rules_rust`, provided as runfiles and put on PATH for just that backend's
    capture_run (design §5/§9 "verifiable slice"). Verified by running the leaf
    with host `python3`/`rustc` removed from PATH.
  * **c, js, zig, swift, hs** still use host toolchains (`cc`, `node`, `zig`,
    `swiftc`, `runghc`). C stays host on purpose: the sanitizer gate needs a cc
    that links ASan, which zig-cc (hermetic_cc) does not (spike, design §5), and
    the C run-leaf compiles every module *instrumented*. zig/swift/hs stay host
    because rules_zig/rules_swift/rules_haskell don't fetch in every build env.

Turning the remaining run leaves into hermetic, remote-cacheable build actions
(compiler/interpreter-in-action) and packaging all of this as reusable
`rules_sudo` macros is the Phase-5 breaking release.
"""

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

def _codegen_impl(ctx):
    sudoc = ctx.executable.sudoc
    out_dir = ctx.actions.declare_directory(ctx.label.name)
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]

    # External backend (e.g. hs): register its manifest with `--external` and
    # run the emitter's toolchain from host PATH. The emit step spawns the
    # backend's own process (`runghc Emit.hs` for hs), which is not a Bazel
    # toolchain, so this codegen action is a no-sandbox `local` action inheriting
    # the host shell env — spec §8 Phase 3 "system-GHC no-sandbox leaf".
    external = "--external " + ctx.attr.manifest if ctx.attr.manifest else ""
    extra_inputs = ctx.files.emitter
    exec_requirements = {"local": "1"} if ctx.attr.manifest else {}

    # An external emitter reads its runtime as data (hs: `readFile "SudoRt.hs"`),
    # which decodes under the *locale* encoding. Bazel build actions run with a
    # sanitized env (no LANG), so GHC defaults to ASCII and chokes on the
    # runtime's UTF-8 bytes. Force a UTF-8 locale for the emitter — portably:
    # glibc (Linux CI) always has C.UTF-8; macOS lacks it but has en_US.UTF-8.
    # (In-tree backends emit via sudoc directly and are unaffected — their
    # command stays byte-identical below.)
    prelude = ""
    if ctx.attr.manifest:
        prelude = (
            "if locale -a 2>/dev/null | grep -qiE '^C\\.utf-?8$'; then\n" +
            "  export LC_ALL=C.UTF-8\n" +
            "else\n" +
            "  export LC_ALL=en_US.UTF-8\n" +
            "fi\n" +
            "export LANG=\"$LC_ALL\"\n"
        )

    command = """
set -euo pipefail
{stage}
{prelude}"{sudoc}" build --target {lang} {external} --tests -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(ctx.files.srcs, stage),
        prelude = prelude,
        sudoc = sudoc.path,
        lang = ctx.attr.lang,
        external = external,
        out = out_dir.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = ctx.files.srcs + extra_inputs,
        outputs = [out_dir],
        tools = [sudoc],
        command = command,
        mnemonic = "SudoCodegen",
        progress_message = "sudoc codegen %{label} (--target " + ctx.attr.lang + ")",
        use_default_shell_env = bool(ctx.attr.manifest),
        execution_requirements = exec_requirements,
    )
    return [DefaultInfo(files = depset([out_dir]), runfiles = ctx.runfiles(files = [out_dir]))]

_lockstep_codegen = rule(
    implementation = _codegen_impl,
    attrs = {
        "srcs": attr.label_list(allow_files = [".sudo"], mandatory = True),
        "entry": attr.string(mandatory = True),
        "lang": attr.string(mandatory = True),
        "sudoc": attr.label(executable = True, cfg = "exec", mandatory = True),
        # External-backend support: `emitter` is the backend's process sources
        # (manifest + emitter script + its imports), `manifest` its
        # workspace-relative path for `--external`. Empty for in-tree backends.
        "emitter": attr.label(allow_files = True),
        "manifest": attr.string(default = ""),
    },
)

def _sudoc_emit_impl(ctx):
    """Shared impl for the manifest + recipe rules: stage srcs, run a sudoc
    emit-* subcommand writing a single JSON file."""
    sudoc = ctx.executable.sudoc
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]

    # emit-recipe for an external backend reads its manifest (the recipe's single
    # source of truth) — pass `--external` and the manifest as an input.
    external = "--external " + ctx.attr.manifest if ctx.attr.manifest else ""

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
"{sudoc}" {subcmd} {targ} {external} -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(ctx.files.srcs, stage),
        sudoc = sudoc.path,
        subcmd = ctx.attr.subcommand,
        targ = "--target " + ctx.attr.lang if ctx.attr.lang else "",
        external = external,
        out = out.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = ctx.files.srcs + ctx.files.emitter,
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
        "srcs": attr.label_list(allow_files = [".sudo"], mandatory = True),
        "entry": attr.string(mandatory = True),
        "subcommand": attr.string(mandatory = True),  # "emit-tests" | "emit-recipe"
        "lang": attr.string(default = ""),  # backend for emit-recipe; empty for emit-tests
        "sudoc": attr.label(executable = True, cfg = "exec", mandatory = True),
        # External-backend support (emit-recipe only): manifest sources + its
        # `--external` path. Empty for in-tree backends and emit-tests.
        "emitter": attr.label(allow_files = True),
        "manifest": attr.string(default = ""),
    },
)

# ---------------------------------------------------------------------------
# Phase 5: `sudo_external_backend` — manifest-free external-backend codegen.
#
# Replaces the `--external`/`discovered_backends()` path with the recipe-JSON-
# from-attrs contract pinned in notes/phase5-external-backend-interface.md:
#   codegen = sudoc emit-ir -> wrap emit-request envelope -> emitter (sh_binary,
#             emit protocol) -> emit_unpack -> generated source dir.
#   recipe  = ctx.actions.write of {build, run} from the recipe_build/recipe_run
#             attrs verbatim (ghc flags incl. -with-rtsopts=-K8m), {entry}->stem.
# The `_lockstep_test` launcher consumes the (dir, recipe.json) pair identically
# to an in-tree backend — it is unchanged.
# ---------------------------------------------------------------------------

def _external_codegen_impl(ctx):
    sudoc = ctx.executable.sudoc
    emitter = ctx.executable.emitter
    emit_unpack = ctx.executable.emit_unpack
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
        _stage_command(ctx.files.srcs, stage),
        '"%s" emit-ir -o "%s/modules.json" "%s/%s"' % (sudoc.path, stage, stage, entry),
        'printf \'{"protocol":2,"cmd":"emit","entry":"%s","with_tests":true,"modules":\' > "%s/request.json"' % (stem, stage),
        'cat "%s/modules.json" >> "%s/request.json"' % (stage, stage),
        'printf \'}\' >> "%s/request.json"' % stage,
        '"%s" < "%s/request.json" > "%s/response.json"' % (emitter.path, stage, stage),
        '"%s" -o "%s" < "%s/response.json"' % (emit_unpack.path, out_dir.path, stage),
    ]) + "\n"

    ctx.actions.run_shell(
        inputs = ctx.files.srcs,
        outputs = [out_dir],
        tools = [sudoc, emitter, emit_unpack],
        command = command,
        mnemonic = "SudoExternalCodegen",
        progress_message = "sudoc external codegen %{label}",
        # The emitter runs the host GHC (`runghc`) — not a Bazel toolchain — so
        # this is a no-sandbox `local` action inheriting host PATH, exactly like
        # the retired `--external` hs codegen leaf.
        use_default_shell_env = True,
        execution_requirements = {"local": "1"},
    )
    return [DefaultInfo(files = depset([out_dir]), runfiles = ctx.runfiles(files = [out_dir]))]

_external_codegen = rule(
    implementation = _external_codegen_impl,
    attrs = {
        "srcs": attr.label_list(allow_files = [".sudo"], mandatory = True),
        "entry": attr.string(mandatory = True),
        "emitter": attr.label(executable = True, cfg = "exec", mandatory = True),
        "sudoc": attr.label(executable = True, cfg = "exec", mandatory = True),
        "emit_unpack": attr.label(executable = True, cfg = "exec", mandatory = True),
    },
)

def _write_recipe_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    ctx.actions.write(output = out, content = ctx.attr.content)
    return [DefaultInfo(files = depset([out]))]

_write_recipe = rule(
    implementation = _write_recipe_impl,
    attrs = {"content": attr.string(mandatory = True)},
)

def sudo_external_backend(
        name,
        srcs,
        entry,
        emitter,
        recipe_build,
        recipe_run,
        sudoc = "//sudoc/crates/cli:sudoc",
        emit_unpack = "//sudoc/crates/harness:emit_unpack",
        **kwargs):
    """Register an external backend via the emit protocol + recipe-from-attrs.

    Produces `<name>` (a generated-source directory) and `<name>_recipe` (a
    TestRecipe JSON), the (codegen, recipe) pair `sudo_lockstep_test`'s launcher
    consumes like any backend. `emitter` is an executable target (IR-JSON ->
    source over the emit protocol); `recipe_build`/`recipe_run` are argv lists
    (list-of-list / list of strings) with `{entry}` substituted to the entry
    stem — the backend's compile/run recipe, verbatim (no manifest, no
    `--external`).
    """
    _external_codegen(
        name = name,
        srcs = srcs,
        entry = entry,
        emitter = emitter,
        sudoc = sudoc,
        emit_unpack = emit_unpack,
        **kwargs
    )
    stem = _entry_stem(entry)
    build_subst = [[a.replace("{entry}", stem) for a in step] for step in recipe_build]
    run_subst = [a.replace("{entry}", stem) for a in recipe_run]
    _write_recipe(
        name = name + "_recipe",
        content = json.encode({"build": build_subst, "run": run_subst}),
    )

def _rf_path(f):
    """Map File.short_path to (kind, path-under-root) for runfiles resolution."""
    sp = f.short_path
    if sp.startswith("../"):
        return ("external", sp[3:])
    return ("workspace", sp)

# Runfiles-resolution bash helper (shared with rules_sudo's launcher pattern).
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

    lines = [
        "#!/usr/bin/env bash",
        "set -uo pipefail",
        "# Generated by sudo_lockstep_test — do not edit.",
        _RF_PRELUDE,
        'CAPTURE="$(rf "%s" "%s")"' % (capture_kind, capture_rel),
        'DIFF="$(rf "%s" "%s")"' % (diff_kind, diff_rel),
        'TESTS="$(rf "%s" "%s")"' % (tests_kind, tests_rel),
        'OUT="${TEST_TMPDIR:-/tmp}"',
        # The run leaves inherit only PATH (tags=local). Give zig an explicit
        # writable cache under the test tmpdir (it can't create its default
        # global cache without a usable HOME). Do NOT override HOME: on CI rustc
        # is a rustup shim that resolves its toolchain via ~/.rustup, so a
        # clobbered HOME breaks the rs backend. swiftc uses TMPDIR for its
        # implicit module cache, so it needs nothing extra.
        'export ZIG_GLOBAL_CACHE_DIR="$OUT/.zig-global-cache"',
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
    # fetchable in every build env; those move in the Phase-5 rules_sudo work.
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
                [ctx.file.tests, ctx.executable.capture_run, ctx.executable.lockstep_diff],
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
        "capture_run": attr.label(executable = True, cfg = "exec", mandatory = True),
        "lockstep_diff": attr.label(executable = True, cfg = "exec", mandatory = True),
    },
    # The py/rs run-leaves use a pinned interpreter/rustc from these toolchains
    # (resolved for the test's target platform, hermetic on Linux and macOS).
    toolchains = [
        "@rules_python//python:toolchain_type",
        "@rules_rust//rust:toolchain_type",
    ],
)

def sudo_lockstep_test(
        name,
        srcs,
        entry,
        backends = ["py", "js"],
        external = {},
        sudoc = "//sudoc/crates/cli:sudoc",
        capture_run = "//sudoc/crates/harness:capture_run",
        lockstep_diff = "//sudoc/crates/harness:lockstep_diff",
        timeout = "long",
        tags = [],
        **kwargs):
    """Lockstep-test one `.sudo` module across `backends` via the decomposed DAG.

    codegen + recipe + tests-manifest are hermetic cached build actions; the run
    leaves execute the per-backend recipe at test time (tags=["local"], PATH
    inherited). py/rs use pinned rules_python/rules_rust toolchains from runfiles;
    c/js/zig/swift/hs use host toolchains (see the module docstring).

    `external` maps an external backend name to `[emitter_label, manifest_path]`
    — its process sources (manifest + emitter script + imports) and the
    workspace-relative manifest path for `--external`. Such a backend's codegen
    runs the emitter from host PATH (no-sandbox `local` action); its run leaf, as
    for every backend, executes the recipe under host toolchains at test time.
    """
    codegens = []
    recipes = []
    for backend in backends:
        gen = "{}_{}_gen".format(name, backend)
        rec = "{}_{}_recipe".format(name, backend)
        ext = external.get(backend)

        # New-form (Phase 5) external backend: a dict
        # {emitter, recipe_build, recipe_run} routed through the manifest-free
        # `sudo_external_backend` rule. Old-form (a [emitter_label, manifest]
        # list) still flows through the `--external` codegen/emit rules below,
        # so both hs paths coexist until the cutover.
        if type(ext) == "dict":
            sudo_external_backend(
                name = gen,
                srcs = srcs,
                entry = entry,
                emitter = ext["emitter"],
                recipe_build = ext["recipe_build"],
                recipe_run = ext["recipe_run"],
                sudoc = sudoc,
            )
            codegens.append(":" + gen)
            recipes.append(":" + gen + "_recipe")
            continue

        emitter = ext[0] if ext else None
        manifest = ext[1] if ext else ""
        _lockstep_codegen(
            name = gen,
            srcs = srcs,
            entry = entry,
            lang = backend,
            sudoc = sudoc,
            emitter = emitter,
            manifest = manifest,
        )
        _lockstep_emit(
            name = rec,
            srcs = srcs,
            entry = entry,
            subcommand = "emit-recipe",
            lang = backend,
            sudoc = sudoc,
            emitter = emitter,
            manifest = manifest,
        )
        codegens.append(":" + gen)
        recipes.append(":" + rec)

    _lockstep_emit(
        name = name + "_tests",
        srcs = srcs,
        entry = entry,
        subcommand = "emit-tests",
        sudoc = sudoc,
    )

    test_tags = list(tags)
    if "local" not in test_tags:
        test_tags.append("local")
    _lockstep_test(
        name = name,
        entry = entry,
        backends = backends,
        codegens = codegens,
        recipes = recipes,
        tests = ":" + name + "_tests",
        capture_run = capture_run,
        lockstep_diff = lockstep_diff,
        timeout = timeout,
        tags = test_tags,
        **kwargs
    )
