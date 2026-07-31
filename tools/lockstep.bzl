"""Decomposed lockstep conformance rules (Bazel migration Phase 1a).

Per `.sudo` module the lockstep DAG is `lockstep_diff(tests-manifest,
N captured-run files)` (design §3):

  * codegen (per backend)  — `sudoc build --target L --tests` → generated source
                             tree. A hermetic, cached build action.
  * tests-manifest         — `sudoc emit-tests` → JSON test-name array. Hermetic,
                             cached. Without it a test absent from every backend
                             would vanish silently.
  * run leaf (per backend) — the generated program under its interpreter, wrapped
                             by `capture_run` (never-fail: always exits 0, writes
                             {stdout, stderr, exit_code}) so a crashing runner
                             still yields a captured file.
  * lockstep_diff          — reads the manifest + N captured files, diffs the
                             per-test outcomes, prints the divergence table, exits
                             nonzero on any divergence/failure.

Phase-1a scope note: codegen and the tests-manifest are hermetic cached build
actions; the **run leaves execute at test time under host interpreters**
(`python3`, `node`) via `env_inherit=["PATH"]` + `tags=["local"]`, matching the
shipped e2e interpreter contract. Making the run leaves hermetic build actions
(interpreter-in-action, remote-cacheable captures) and a hermetic `node` toolchain
are deferred to Phase 1b (rules_nodejs is incompatible with Bazel 9). Packaging
these as reusable `rules_sudo` macros is the Phase-5 breaking release.
"""

# backend -> (interpreter argv0, generated-entry file extension)
_BACKENDS = {
    "py": ("python3", ".py"),
    "js": ("node", ".mjs"),
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
        "lang": attr.string(mandatory = True, values = _BACKENDS.keys()),
        "sudoc": attr.label(executable = True, cfg = "exec", mandatory = True),
    },
)

def _manifest_impl(ctx):
    sudoc = ctx.executable.sudoc
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    stage = "_stage_{}".format(ctx.label.name)
    entry = ctx.attr.entry.split("/")[-1]
    command = """
set -euo pipefail
{stage}
"{sudoc}" emit-tests -o "{out}" "{stagedir}/{entry}"
""".format(
        stage = _stage_command(ctx.files.srcs, stage),
        sudoc = sudoc.path,
        out = out.path,
        stagedir = stage,
        entry = entry,
    )
    ctx.actions.run_shell(
        inputs = ctx.files.srcs,
        outputs = [out],
        tools = [sudoc],
        command = command,
        mnemonic = "SudoTestsManifest",
        progress_message = "sudoc emit-tests %{label}",
    )
    return [DefaultInfo(files = depset([out]))]

_lockstep_manifest = rule(
    implementation = _manifest_impl,
    attrs = {
        "srcs": attr.label_list(allow_files = [".sudo"], mandatory = True),
        "entry": attr.string(mandatory = True),
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
        "DIFF_ARGS=()",
    ]

    # ctx.attr.codegens and ctx.attr.backends are parallel lists.
    for i in range(len(ctx.attr.backends)):
        backend = ctx.attr.backends[i]
        interp, ext = _BACKENDS[backend]
        tree = ctx.files.codegens[i]  # the declared directory File
        kind, rel = _rf_path(tree)
        impl = "_{}_impl{}".format(entry_stem, ext)
        lines += [
            '# --- backend %s ---' % backend,
            'GEN="$(rf "%s" "%s")"' % (kind, rel),
            '( cd "$GEN" && "$CAPTURE" --out "$OUT/%s.json" -- %s "%s" )' % (backend, interp, impl),
            'DIFF_ARGS+=(--run %s="$OUT/%s.json")' % (backend, backend),
        ]

    lines += [
        'exec "$DIFF" --module "%s" --tests "$TESTS" "${DIFF_ARGS[@]}"' % entry_stem,
    ]

    launcher = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(output = launcher, content = "\n".join(lines) + "\n", is_executable = True)

    runfiles = ctx.runfiles(
        files = ctx.files.codegens + [ctx.file.tests, ctx.executable.capture_run, ctx.executable.lockstep_diff],
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

    codegen + tests-manifest are hermetic cached build actions; the run leaves
    execute at test time under host interpreters (tags=["local"], PATH inherited).
    """
    codegens = []
    for backend in backends:
        gen = "{}_{}_gen".format(name, backend)
        _lockstep_codegen(name = gen, srcs = srcs, entry = entry, lang = backend, sudoc = sudoc)
        codegens.append(":" + gen)

    _lockstep_manifest(name = name + "_tests", srcs = srcs, entry = entry, sudoc = sudoc)

    test_tags = list(tags)
    if "local" not in test_tags:
        test_tags.append("local")
    _lockstep_test(
        name = name,
        entry = entry,
        backends = backends,
        codegens = codegens,
        tests = ":" + name + "_tests",
        capture_run = capture_run,
        lockstep_diff = lockstep_diff,
        timeout = timeout,
        tags = test_tags,
        **kwargs
    )
