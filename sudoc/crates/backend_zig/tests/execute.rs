//! Execution tests: generated Zig compiles with `zig build-exe` and its sudo
//! tests pass. Mirrors backend_rs/tests/execute.rs, but drives the Zig
//! toolchain and the multi-module (`{module}.zig` + `sudo_rt.zig`) layout.

use std::path::Path;
use std::process::Command;

fn zig_available() -> bool {
    Command::new("zig")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_generated(name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("sudoc-zigtest-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, name).expect("checks");
    let code = sudoc_backend_zig::emit(&ir, true, true);
    let entry = format!("{name}.zig");
    std::fs::write(dir.join(&entry), &code).unwrap();
    std::fs::write(
        dir.join(sudoc_backend_zig::RUNTIME_FILE),
        sudoc_backend_zig::RUNTIME,
    )
    .unwrap();
    let cc = Command::new("zig")
        .current_dir(&dir)
        .args([
            "build-exe",
            &entry,
            "-femit-bin=sudo_tests",
            "-lc",
            "-O",
            "ReleaseSafe",
        ])
        .output()
        .expect("zig runs");
    assert!(
        cc.status.success(),
        "{name}: zig build-exe failed:\n{}\n--- generated ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&cc.stdout),
        code,
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(dir.join("sudo_tests"))
        .current_dir(&dir)
        .output()
        .expect("binary runs");
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
fn all_examples_pass_their_tests_in_zig() {
    if !zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut checked = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        if name == "order_dependent" {
            // May pass or fail depending on Map iteration order; must compile+run.
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
    if !zig_available() {
        return;
    }
    let out = run_generated("failing", "test \"fails\"\n    assert 1 == 2\n");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fails"), "{stdout}");
    assert!(stdout.contains("AssertFailed"), "{stdout}");
}

#[test]
fn value_semantics_hold_at_runtime() {
    if !zig_available() {
        return;
    }
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
    assert_passes("semantics", src);
}

#[test]
fn overflow_traps() {
    if !zig_available() {
        return;
    }
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
    if !zig_available() {
        return;
    }
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

#[test]
fn expect_trap_observes_kind() {
    if !zig_available() {
        return;
    }
    let src = r#"test "catches oob"
    expect_trap OutOfBounds
        a = [1]
        x = a[5]
        assert x == 0

test "catches key missing"
    expect_trap KeyMissing
        m = Map()
        m[1] = 10
        v = m[2]
        assert v == 0
"#;
    assert_passes("expect", src);
}

#[test]
fn maps_and_sets_structural_keys() {
    if !zig_available() {
        return;
    }
    let src = r#"test "list keys and set dedup"
    m = Map()
    m[[1, 2]] = 10
    m[[3]] = 20
    assert m[[1, 2]] == 10
    assert m.get([9]) == None
    assert m.size == 2
    s = Set()
    s.add(1)
    s.add(1)
    s.add(2)
    assert s.size == 2
    assert s.has(2)
"#;
    assert_passes("maps", src);
}

/// Composite module constants are `pub var` filled by `sudoInitConsts()` from
/// `main()` before any test runs — proves the init path end-to-end.
#[test]
fn composite_module_consts_initialized() {
    if !zig_available() {
        return;
    }
    let src = r#"NUMS: List<int> = [10, 20, 30]
LIMIT = 100
BASE: List<int> = [1, 2]
ALIAS: List<int> = BASE

test "sum list const"
    total = 0
    for n in NUMS
        total = total + n
    assert total == 60

test "scalar still folded"
    assert LIMIT == 100

test "const ref is independent value"
    assert ALIAS == [1, 2]
    assert BASE == [1, 2]
    local = ALIAS
    local.append(3)
    assert ALIAS == [1, 2]
    assert local == [1, 2, 3]
"#;
    assert_passes("compconst", src);
}

#[test]
fn for_range_to_i64_max_terminates() {
    if !zig_available() {
        return;
    }
    let src = r#"test "range to max"
    big = 9223372036854775807
    count = 0
    for i = big to big
        count = count + 1
    assert count == 1
"#;
    assert_passes("for_max", src);
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
