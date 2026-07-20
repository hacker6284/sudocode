//! Execution tests: generated C compiles with cc and its sudo tests pass.
//! This is the M3 conformance gate — the same sources the Python backend
//! passes, now through manual memory and monomorphized types.

use std::path::Path;
use std::process::Command;

fn run_generated(name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("sudoc-ctest-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, name).expect("checks");
    let code = sudoc_backend_c::emit(&ir, true);
    let c_path = dir.join(format!("{name}.c"));
    std::fs::write(&c_path, &code).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_H_FILE), sudoc_backend_c::RUNTIME_H).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_C_FILE), sudoc_backend_c::RUNTIME_C).unwrap();
    let bin = dir.join("prog");
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&bin)
        .arg(&c_path)
        .arg(dir.join(sudoc_backend_c::RUNTIME_C_FILE))
        .output()
        .expect("cc runs");
    assert!(
        cc.status.success(),
        "{name}: cc failed:\n{}\n--- generated C ---\n{}",
        String::from_utf8_lossy(&cc.stderr),
        code
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
fn all_examples_pass_their_tests_in_c() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut checked = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        // The deliberately order-dependent pitfall may pass or trap in C —
        // its divergence is M4's business. Skip the pass/fail assertion.
        let src = std::fs::read_to_string(&path).unwrap();
        if name == "order_dependent" {
            run_generated(&name, &src); // still must compile and run
        } else {
            assert_passes(&name, &src);
        }
        checked += 1;
    }
    assert!(checked >= 9);
}

#[test]
fn value_semantics_hold_in_c() {
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
fn hoisted_inout_calls_behave_in_c() {
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

test "while condition re-evaluates"
    n = 0
    total = 0
    while take(n) <= 30
        total = total + 1
    assert n == 4
    assert total == 3
"#;
    assert_passes("hoisting", src);
}

#[test]
fn overflow_traps_in_c() {
    let src = r#"test "overflow is loud"
    big = 9223372036854775807
    x = big + 1
    assert x == 0

test "min int abs overflows"
    min_int = -9223372036854775807 - 1
    y = abs(min_int)
    assert y == 0
"#;
    let out = run_generated("overflowc", src);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("[Overflow]").count(), 2, "{stdout}");
}

#[test]
fn traps_carry_kinds_in_c() {
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
fn floats_follow_ieee_in_c() {
    let src = r#"test "ieee edges"
    nan = 0.0 / 0.0
    assert not (nan == nan)
    inf = 1.0 / 0.0
    assert inf > 0.0
    assert not (min(nan, 1.0) == min(nan, 1.0))
    assert round(2.5) == 3.0
    assert round(-2.5) == -3.0
    assert floor(-1.5) == -2.0
    assert int(2.9) == 2
    assert int(-2.9) == -2

test "list of float sorts totally"
    xs = [0.0 - 0.0, 3.0, 0.0 / 0.0, 1.0]
    xs.sort()
    assert xs[1] == 1.0
    assert xs[2] == 3.0
    assert not (xs[3] == xs[3])
"#;
    // `min(nan, 1.0) != min(nan, 1.0)` because NaN != NaN.
    assert_passes("floats", src);
}

#[test]
fn records_enums_maps_work_in_c() {
    let src = r#"record Point
    x: int
    y: int

enum Shape
    Dot
    Rect(w: int, h: int, at: Point)

func area(s: Shape) -> int
    match s
        case Dot
            return 0
        case Rect(w, h, at)
            return w * h + at.x * 0

test "records and enums"
    p = Point(3, 4)
    q = p
    q.x = 100
    assert p.x == 3
    s = Rect(2, 5, p)
    assert area(s) == 10
    assert area(Dot) == 0

test "maps with structural keys"
    m = Map()
    m[[1, 2]] = 10
    m[[3]] = 20
    assert m[[1, 2]] == 10
    assert m.get([9]) == None
    assert m.size == 2
    assert m.delete([3])
    assert m.size == 1

test "option round trip"
    o = Some((1, 2))
    match o
        case Some(pair)
            a, b = pair
            assert a + b == 3
        case None
            assert false
"#;
    assert_passes("structures", src);
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
fn traps_do_not_leak() {
    // macOS-only verification via `leaks`; the arena itself is portable.
    if Command::new("which").arg("leaks").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: `leaks` tool unavailable");
        return;
    }
    let src = r#"func build(n: int) -> List<List<int>>
    out: List<List<int>> = []
    for i = 0 to n - 1
        row = [i, i, i]
        out.append(row)
    return out

test "allocates then traps"
    big = build(50)
    m = Map()
    m[[1, 2]] = build(10)
    x = big[999]
    assert x == []

test "richer trap"
    t = "some text we will lose"
    u = t + t
    assert u[5000] == 0
"#;
    let dir = std::env::temp_dir().join(format!("sudoc-leaktrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, "leaktrap").expect("checks");
    let code = sudoc_backend_c::emit(&ir, true);
    std::fs::write(dir.join("leaktrap.c"), &code).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_H_FILE), sudoc_backend_c::RUNTIME_H).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_C_FILE), sudoc_backend_c::RUNTIME_C).unwrap();
    let bin = dir.join("prog");
    let cc = Command::new("cc")
        .args(["-std=c11", "-o"])
        .arg(&bin)
        .arg(dir.join("leaktrap.c"))
        .arg(dir.join(sudoc_backend_c::RUNTIME_C_FILE))
        .output()
        .unwrap();
    assert!(cc.status.success(), "{}", String::from_utf8_lossy(&cc.stderr));
    let leaks = Command::new("leaks").args(["--atExit", "--"]).arg(&bin).output().unwrap();
    let out = String::from_utf8_lossy(&leaks.stdout);
    assert!(
        out.contains("0 leaks for 0 total leaked bytes"),
        "trap leaked memory:\n{}",
        out.lines().filter(|l| l.contains("leak")).collect::<Vec<_>>().join("\n")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn host_adapters_speak_c() {
    let src = r#"export func shout(s: text) -> text
    out = ""
    for c in s
        if c >= 'a' and c <= 'z'
            out.append(c - 32)
        else
            out.append(c)
    return out

export func gcd(a: int, b: int) -> int
    while b != 0
        a, b = b, a mod b
    return abs(a)

export func sorted_copy(items: inout List<int>)
    items.sort()

export func nth(items: List<int>, i: int) -> int
    return items[i]
"#;
    let host_main = r#"
#include "adapters.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

int main(void) {
    char *up = NULL;
    assert(adapters_shout("hello!", &up) == SUDO_OK);
    assert(strcmp(up, "HELLO!") == 0);
    free(up);

    int64_t g = 0;
    assert(adapters_gcd(12, 18, &g) == SUDO_OK);
    assert(g == 6);

    int64_t xs[] = {3, 1, 2};
    int64_t *sorted = NULL;
    int64_t n = 0;
    assert(adapters_sorted_copy(xs, 3, &sorted, &n) == SUDO_OK);
    assert(n == 3 && sorted[0] == 1 && sorted[2] == 3);
    free(sorted);

    int64_t v = 0;
    assert(adapters_nth(xs, 3, 99, &v) == SUDO_TRAP_OUT_OF_BOUNDS);
    assert(adapters_nth(xs, 3, 1, &v) == SUDO_OK && v == 1);

    printf("HOST OK\n");
    return 0;
}
"#;
    let dir = std::env::temp_dir().join(format!("sudoc-cadapters-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(src, "adapters").expect("checks");
    std::fs::write(dir.join("adapters.c"), sudoc_backend_c::emit(&ir, false)).unwrap();
    let header = sudoc_backend_c::emit_header(&ir).expect("has adaptable exports");
    std::fs::write(dir.join("adapters.h"), header).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_H_FILE), sudoc_backend_c::RUNTIME_H).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_C_FILE), sudoc_backend_c::RUNTIME_C).unwrap();
    std::fs::write(dir.join("main.c"), host_main).unwrap();
    let bin = dir.join("host");
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&bin)
        .arg(dir.join("main.c"))
        .arg(dir.join("adapters.c"))
        .arg(dir.join(sudoc_backend_c::RUNTIME_C_FILE))
        .output()
        .unwrap();
    assert!(cc.status.success(), "cc failed:\n{}", String::from_utf8_lossy(&cc.stderr));
    let out = Command::new(&bin).output().unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("HOST OK"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}
