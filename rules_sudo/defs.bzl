"""Public API. Load rules from here, not from private/."""

load(
    "//private:rules.bzl",
    _sudo_js_library = "sudo_js_library",
    _sudo_library = "sudo_library",
    _sudo_lockstep_test = "sudo_lockstep_test",
    _sudo_py_library = "sudo_py_library",
    _SudoInfo = "SudoInfo",
)

SudoInfo = _SudoInfo
sudo_library = _sudo_library
sudo_py_library = _sudo_py_library
sudo_js_library = _sudo_js_library

def sudo_lockstep_test(name, timeout = "long", **kwargs):
    """Lockstep runs every test in every target — minutes, not seconds.

    Bazel's default moderate (300s) timeout is exactly the wrong size for a
    multi-target suite on a CI runner, so the default here is long (900s).
    """
    _sudo_lockstep_test(
        name = name,
        timeout = timeout,
        **kwargs
    )
