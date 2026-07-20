//! Execution tests: generated Rust compiles with rustc and its sudo tests pass.

use std::path::Path;
use std::process::Command;

fn run_generated(name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("sudoc-rstest-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, name).expect("checks");
    let code = sudoc_backend_rs::emit(&ir, true, true);
    let entry = format!("{name}.rs");
    std::fs::write(dir.join(&entry), &code).unwrap();
    std::fs::write(
        dir.join(sudoc_backend_rs::RUNTIME_FILE),
        sudoc_backend_rs::RUNTIME,
    )
    .unwrap();
    let bin = dir.join("sudo_tests");
    let cc = Command::new("rustc")
        .current_dir(&dir)
        .args([
            "--edition",
            "2021",
            "-A",
            "warnings",
            "-o",
            "sudo_tests",
            &entry,
        ])
        .output()
        .expect("rustc runs");
    assert!(
        cc.status.success(),
        "{name}: rustc failed:\n{}\n--- generated ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&cc.stdout),
        code,
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(&bin).output().expect("binary runs");
    std::fs::remove_dir_all(&dir).ok();
    out
}

fn assert_passes(name: &str, src: &str) {
    let out = run_generated(name, src);
    assert!(
        out.status.success(),
        "{name} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn all_examples_pass_their_tests_in_rust() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut checked = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        if name == "order_dependent" {
            // May pass or fail depending on HashMap iteration; must still compile+run.
            let _ = run_generated(&name, &src);
        } else {
            assert_passes(&name, &src);
        }
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
fn for_range_to_i64_max_terminates() {
    let src = r#"test "range to max"
    big = 9223372036854775807
    count = 0
    for i = big to big
        count = count + 1
    assert count == 1
"#;
    let out = run_generated("for_max", src);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
