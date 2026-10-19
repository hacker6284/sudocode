//! Behavioral coverage for ranged ints: every in-tree backend must trap
//! `Range` on an out-of-range store, keep a provably-in-range store exact,
//! and leave plain `int` overflow as `Overflow`.

use std::path::PathBuf;
use std::process::Command;

use sudoc_harness::{all_backends, lockstep, Backend};

fn available() -> Vec<Box<dyn Backend>> {
    all_backends()
        .into_iter()
        .filter(|b| match b.name() {
            "swift" => Command::new("swiftc").arg("--version").output().is_ok(),
            "zig" => Command::new("zig").arg("version").output().is_ok(),
            _ => true,
        })
        .collect()
}

fn write_module(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-range-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

const SRC: &str = r#"
type Coord = int in -1..1

record P
    x: Coord
    y: int in 0..10

func clamp_add(a: int in 0..100, b: int in 0..100) -> int in 0..200
    return a + b

func overflow_add(a: int in 0..100, b: int in 0..100) -> int in 0..150
    return a + b

func take(x: int) -> int
    return x + 1

func give(x: int) -> int in 0..10
    return x

func set_elem(xs: inout List<int in 0..5>, i: int, v: int)
    xs[i] = v

func half(x: int in 0..10) -> int
    return x / 2

func residue(x: int in -20..20) -> int
    return x mod 5

test "in range"
    c: Coord = -1
    assert c == -1
    p = P(1, 10)
    assert p.x == 1
    assert p.y == 10
    assert clamp_add(100, 100) == 200
    assert take(c) == 0
    n: int = 3
    assert give(n) == 3
    xs: List<int in 0..5> = [0, 5]
    xs.append(4)
    set_elem(xs, 0, 2)
    assert xs[0] == 2
    t: (int in -1..1, int) = (-1, 99)
    a, b = t
    assert a == -1
    assert b == 99
    assert half(9) == 4
    assert residue(-3) == 2

test "assign trap"
    expect_trap Range
        n = 2
        c: Coord = n

test "field trap"
    expect_trap Range
        n = 11
        p = P(0, n)

test "param trap"
    expect_trap Range
        n = 101
        x = clamp_add(n, 0)

test "return trap"
    expect_trap Range
        n = 11
        x = give(n)

test "element trap"
    expect_trap Range
        xs: List<int in 0..5> = [0]
        n = 6
        xs.append(n)

test "index trap"
    expect_trap Range
        xs: List<int in 0..5> = [0]
        n = 9
        set_elem(xs, 0, n)

test "proven sum still traps a tighter return"
    expect_trap Range
        x = overflow_add(100, 100)

test "plain i64 overflow"
    expect_trap Overflow
        big = 9223372036854775807
        x = big + 1

test "plain i64 neg"
    expect_trap Overflow
        m = -9223372036854775808
        x = 0 - m
"#;

#[test]
fn ranged_int_lockstep() {
    let backs = available();
    let names: Vec<_> = backs.iter().map(|b| b.name().to_string()).collect();
    assert!(
        names.iter().any(|n| n == "js") && names.iter().any(|n| n == "py"),
        "expected the in-tree backends, got {names:?}"
    );
    let path = write_module("range_int", SRC);
    let report = lockstep(&path, &backs).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}
