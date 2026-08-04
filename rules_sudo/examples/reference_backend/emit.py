"""Minimal reference external-backend emitter (rules_sudo worked example, spec
§7 / §2.7). Speaks the emit protocol (spec/protocol.md §2): reads ONE emit-request
envelope on stdin, writes ONE {"files":[...]} response on stdout.

It "transpiles" a checked sudo program to a tiny Python test script. To stay a
minimal skeleton — NOT gold-plated — it supports only modules whose tests all
pass and whose names are identifier-safe (it does not evaluate assertions; it
emits `ok` for every test). That is enough to lockstep-agree with the built-in
backends on the demo module and to demonstrate the whole plugin surface: an
`sh_binary` emitter + a `sudo_external_backend` descriptor + a
`sudo_lockstep_test` that references it by label. A real backend would walk each
test's `body` (the IR expressions shown in `sudoc emit-ir`) and emit code that
actually checks the assertions, printing `not ok N - name [TrapKind]` on failure.
"""

import json
import sys


def main() -> None:
    req = json.load(sys.stdin)
    entry = req["entry"]
    # Modules come dependency-first, entry last; the entry module carries tests.
    tests = req["modules"][-1].get("tests", [])
    # The tests-manifest (sudoc emit-tests) names each test `test_<name>`; the
    # captured TAP must use the same names so lockstep_diff can align outcomes.
    lines = [
        'print("ok {} - test_{}")'.format(i, t["name"])
        for i, t in enumerate(tests, start=1)
    ]
    program = "\n".join(lines) + "\n"
    response = {"files": [{"path": entry + "_test.py", "contents": program}]}
    json.dump(response, sys.stdout)


if __name__ == "__main__":
    main()
