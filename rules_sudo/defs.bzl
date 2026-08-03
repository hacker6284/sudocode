"""Public API. Load rules from here, not from private/."""

load(
    "//private:lockstep.bzl",
    _SudoBackendInfo = "SudoBackendInfo",
    _sudo_external_backend = "sudo_external_backend",
    _sudo_lockstep_test = "sudo_lockstep_test",
)
load(
    "//private:rules.bzl",
    _sudo_js_library = "sudo_js_library",
    _sudo_library = "sudo_library",
    _sudo_py_library = "sudo_py_library",
    _SudoInfo = "SudoInfo",
)

SudoInfo = _SudoInfo
sudo_library = _sudo_library
sudo_py_library = _sudo_py_library
sudo_js_library = _sudo_js_library

# Phase 5 (rules_sudo 1.0.0, compatibility_level 2): the hermetic decomposed
# lockstep + standalone external-backend registration. `sudo_lockstep_test` is
# LIB-BASED and REPLACES the v0.2.1 PATH-inheriting monolithic test (breaking).
SudoBackendInfo = _SudoBackendInfo
sudo_external_backend = _sudo_external_backend
sudo_lockstep_test = _sudo_lockstep_test
