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
sudo_lockstep_test = _sudo_lockstep_test
