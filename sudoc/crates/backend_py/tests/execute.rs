//! Execution tests: generated Python actually runs and its sudo tests pass
//! under python3. This is the M2 conformance gate.

use std::path::Path;
use std::process::Command;

fn run_generated(name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("sudoc-pytest-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, name).expect("checks");
    let code = sudoc_backend_py::emit(&ir, true);
    std::fs::write(dir.join(format!("{name}.py")), &code).unwrap();
    std::fs::write(dir.join(sudoc_backend_py::RUNTIME_FILE), sudoc_backend_py::RUNTIME).unwrap();
    let out = Command::new("python3")
        .arg(dir.join(format!("{name}.py")))
        .output()
        .expect("python3 runs");
    std::fs::remove_dir_all(&dir).ok();
    out
}

#[test]
fn all_examples_pass_their_tests_in_python() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut checked = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        let out = run_generated(&name, &src);
        assert!(
            out.status.success(),
            "{name} failed under python3:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        checked += 1;
    }
    assert!(checked >= 9);
}

#[test]
fn failing_assert_exits_nonzero() {
    let out = run_generated("failing", "test \"fails\"\n    assert 1 == 2\n");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fails"), "{stdout}");
}

#[test]
fn value_semantics_hold_at_runtime() {
    // b = a must be an independent copy; quicksort-style inout must mutate.
    let src = r#"func mutate(items: inout List<int>)
    items.append(99)

test "assignment copies"
    a = [1, 2]
    b = a
    b.append(3)
    assert a == [1, 2]
    assert b == [1, 2, 3]

test "inout mutates caller"
    a = [1]
    mutate(a)
    assert a == [1, 99]

test "floor division"
    assert -7 / 2 == -4
    assert -7 mod 2 == 1
    assert 7 mod -2 == -1
"#;
    let out = run_generated("semantics", src);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn overflow_traps() {
    let src = r#"test "overflow is loud"
    big = 9223372036854775807
    x = big + 1
    assert x == 0
"#;
    let out = run_generated("overflow", src);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Overflow"), "{stdout}");
}

#[test]
fn traps_carry_kinds() {
    let src = r#"test "oob traps"
    a = [1]
    x = a[5]
    assert x == 0
"#;
    let out = run_generated("traps", src);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("OutOfBounds"), "{stdout}");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("examples dir exists") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "sudo") {
            out.push(path);
        }
    }
    out
}

#[test]
fn hoisted_inout_calls_behave() {
    let src = r#"func take(x: inout int) -> int
    x = x + 1
    return x * 10

func noisy(x: inout int) -> bool
    x = x + 1
    return true

test "nested in expression"
    n = 0
    y = take(n) + 1
    assert y == 11
    assert n == 1

test "in a return position"
    n = 5
    assert ret_helper() == 61

test "left to right order"
    n = 0
    y = take(n) + take(n)
    assert y == 30
    assert n == 2

test "short circuit skips mutation"
    n = 0
    b = true or noisy(n)
    assert b
    assert n == 0
    c = false and noisy(n)
    assert not c
    assert n == 0

test "while condition re-evaluates"
    n = 0
    total = 0
    while take(n) <= 30
        total = total + 1
    assert n == 4
    assert total == 3

func ret_helper() -> int
    n = 5
    return take(n) + 1
"#;
    let out = run_generated("hoisting", src);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn host_adapters_speak_python() {
    // Exports get host-facing wrappers: str <-> text, Option collapse,
    // Result raising, in-place inout writeback, input validation.
    let src = r#"export func shout(s: text) -> text
    out = ""
    for c in s
        if c >= 'a' and c <= 'z'
            out.append(c - 32)
        else
            out.append(c)
    return out

export func find(items: List<int>, t: int) -> Option<int>
    for i = 0 to items.length - 1
        if items[i] == t
            return Some(i)
    return None

export func safe_div(a: int, b: int) -> Result<int, text>
    if b == 0
        return Err("division by zero")
    return Ok(a / b)

export func push_twice(items: inout List<int>, v: int)
    items.append(v)
    items.append(v)

export func total(m: Map<text, int>) -> int
    sum = 0
    for k, v in m
        sum = sum + v
    return sum
"#;
    let driver = r#"
import adapters as lib
from _sudo_rt import SudoTrap

# text maps to str both ways.
assert lib.shout("hello!") == "HELLO!", lib.shout("hello!")

# Option collapses to value-or-None.
assert lib.find([5, 6, 7], 6) == 1
assert lib.find([5, 6, 7], 9) is None

# Result unwraps or raises SudoError with the converted payload.
assert lib.safe_div(10, 2) == 5
try:
    lib.safe_div(1, 0)
    raise SystemExit("expected SudoError")
except Exception as e:
    assert type(e).__name__ == "SudoError", type(e).__name__
    assert "division by zero" in str(e), str(e)

# inout writes back into the host's own list, in place.
xs = [1]
lib.push_twice(xs, 9)
assert xs == [1, 9, 9], xs

# Host dicts convert in; text keys become str internally.
assert lib.total({"a": 1, "b": 2}) == 3

# Validation: out-of-range ints raise ValueError, not a trap.
try:
    lib.find([1], 2**70)
    raise SystemExit("expected ValueError")
except ValueError:
    pass

# Traps surface as SudoTrap with a kind.
try:
    lib.safe_div(-9223372036854775807 - 1, -1)  # MIN / -1 overflows
    raise SystemExit("expected SudoTrap")
except SudoTrap as t:
    assert t.kind == "Overflow", t.kind

print("HOST OK")
"#;
    let dir = std::env::temp_dir().join(format!("sudoc-pyadapters-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, "adapters").expect("checks");
    std::fs::write(dir.join(sudoc_backend_py::impl_file("adapters")), sudoc_backend_py::emit(&ir, false)).unwrap();
    let api = sudoc_backend_py::emit_api(&ir).expect("has exports");
    std::fs::write(dir.join(sudoc_backend_py::api_file("adapters")), api).unwrap();
    std::fs::write(dir.join(sudoc_backend_py::RUNTIME_FILE), sudoc_backend_py::RUNTIME).unwrap();
    std::fs::write(dir.join("driver.py"), driver).unwrap();
    let out = Command::new("python3").arg(dir.join("driver.py")).output().unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("HOST OK"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}
