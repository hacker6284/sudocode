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

Phase-1a/1b scope note: codegen, recipe and the tests-manifest are hermetic
cached build actions; the **run leaves execute at test time under host
toolchains** (`python3`, `node`, `cc`, `rustc`, `zig`) via `env_inherit=["PATH"]`
+ `tags=["local"]`. Making the run leaves hermetic build actions
(interpreter/compiler-in-action, remote-cacheable captures) is a tracked
follow-up (needs hermetic toolchains — hermetic_cc/rules_zig verified to work on
Bazel 8.3.1, and a Bazel-8-compatible hermetic node). Packaging these as reusable
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
    command = """
set -euo pipefail
{stage}
"{sudoc}" build --target {lang} --tests -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(ctx.files.srcs, stage),
        sudoc = sudoc.path,
        lang = ctx.attr.lang,
        out = out_dir.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = ctx.files.srcs,
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
        "srcs": attr.label_list(allow_files = [".sudo"], mandatory = True),
        "entry": attr.string(mandatory = True),
        "lang": attr.string(mandatory = True),
        "sudoc": attr.label(executable = True, cfg = "exec", mandatory = True),
    },
)

def _sudoc_emit_impl(ctx):
    """Shared impl for the manifest + recipe rules: stage srcs, run a sudoc
    emit-* subcommand writing a single JSON file."""
    sudoc = ctx.executable.sudoc
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]
    command = """
set -euo pipefail
{stage}
"{sudoc}" {subcmd} {targ} -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(ctx.files.srcs, stage),
        sudoc = sudoc.path,
        subcmd = ctx.attr.subcommand,
        targ = "--target " + ctx.attr.lang if ctx.attr.lang else "",
        out = out.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = ctx.files.srcs,
        outputs = [out],
        tools = [sudoc],
        command = command,
        mnemonic = "SudoEmit",
        progress_message = "sudoc " + ctx.attr.subcommand + " %{label}",
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
    },
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

    # codegens[i] and recipes[i] are parallel to backends[i].
    for i in range(len(ctx.attr.backends)):
        backend = ctx.attr.backends[i]
        gen_kind, gen_rel = _rf_path(ctx.files.codegens[i])
        rec_kind, rec_rel = _rf_path(ctx.files.recipes[i])
        lines += [
            '# --- backend %s ---' % backend,
            'GEN="$(rf "%s" "%s")"' % (gen_kind, gen_rel),
            'RECIPE="$(rf "%s" "%s")"' % (rec_kind, rec_rel),
            'W="$OUT/%s_work"' % backend,
            'rm -rf "$W"; cp -r "$GEN" "$W"; chmod -R u+w "$W"',
            '"$CAPTURE" --recipe "$RECIPE" --dir "$W" --out "$OUT/%s.json"' % backend,
            'DIFF_ARGS+=(--run %s="$OUT/%s.json")' % (backend, backend),
        ]

    lines.append('exec "$DIFF" --module "%s" --tests "$TESTS" "${DIFF_ARGS[@]}"' % entry_stem)

    launcher = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(output = launcher, content = "\n".join(lines) + "\n", is_executable = True)

    runfiles = ctx.runfiles(
        files = ctx.files.codegens + ctx.files.recipes +
                [ctx.file.tests, ctx.executable.capture_run, ctx.executable.lockstep_diff],
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
)

def sudo_lockstep_test(
        name,
        srcs,
        entry,
        backends = ["py", "js"],
        sudoc = "//sudoc/crates/cli:sudoc",
        capture_run = "//sudoc/crates/harness:capture_run",
        lockstep_diff = "//sudoc/crates/harness:lockstep_diff",
        timeout = "long",
        tags = [],
        **kwargs):
    """Lockstep-test one `.sudo` module across `backends` via the decomposed DAG.

    codegen + recipe + tests-manifest are hermetic cached build actions; the run
    leaves execute the per-backend recipe at test time under host toolchains
    (tags=["local"], PATH inherited).
    """
    codegens = []
    recipes = []
    for backend in backends:
        gen = "{}_{}_gen".format(name, backend)
        rec = "{}_{}_recipe".format(name, backend)
        _lockstep_codegen(name = gen, srcs = srcs, entry = entry, lang = backend, sudoc = sudoc)
        _lockstep_emit(
            name = rec,
            srcs = srcs,
            entry = entry,
            subcommand = "emit-recipe",
            lang = backend,
            sudoc = sudoc,
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
