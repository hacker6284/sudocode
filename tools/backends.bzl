"""Shared backend list + dogfood wrapper for the lockstep BUILD files
(conformance, conformance/multimodule, stdlib, examples).

Phase 5 Task 6: the seven-backend list and the four toolchain-binary overrides
were copy-pasted into every lockstep BUILD file. They live here now, behind one
`dogfood_lockstep_test` wrapper, so a change (a new backend, a moved binary)
touches one file.
"""

load("@rules_sudo//:defs.bzl", _sudo_lockstep_test = "sudo_lockstep_test")

# All seven backends. hs is the standalone external-backend descriptor
# (`sudo_external_backend`), referenced by label; the rest are in-tree language
# names resolved inside @rules_sudo.
ALL_BACKENDS = ["py", "js", "c", "rs", "zig", "swift", "//backends/haskell:hs"]

# sudocode dogfoods @rules_sudo, overriding the macro's toolchain-binary attrs to
# its freshly-built first-party binaries — so @sudo_toolchain (a release fetch)
# is never instantiated here (no chicken-and-egg; design §4/§6).
_SUDOC = "//sudoc/crates/cli:sudoc"
_CAPTURE_RUN = "//sudoc/crates/harness:capture_run"
_LOCKSTEP_DIFF = "//sudoc/crates/harness:lockstep_diff"
_EMIT_UNPACK = "//sudoc/crates/harness:emit_unpack"

def dogfood_lockstep_test(name, lib, entry, backends = ALL_BACKENDS, **kwargs):
    """`sudo_lockstep_test` with sudocode's first-party binaries wired in.

    Defaults to all seven backends. Pass `backends` to override (e.g. a module a
    backend can't yet express).
    """
    _sudo_lockstep_test(
        name = name,
        lib = lib,
        entry = entry,
        backends = backends,
        sudoc = _SUDOC,
        capture_run = _CAPTURE_RUN,
        lockstep_diff = _LOCKSTEP_DIFF,
        emit_unpack = _EMIT_UNPACK,
        **kwargs
    )
