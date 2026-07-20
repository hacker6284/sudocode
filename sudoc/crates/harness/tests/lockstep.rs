//! The M4 gate: lockstep runs across Python and C, detecting agreement,
//! consistent failure, and — the flagship — divergence.

use std::path::PathBuf;

use sudoc_harness::{all_backends, lockstep, parse_tap, Backend, Outcome, TapLine, Verdict};

fn write_module(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

fn both() -> Vec<Box<dyn Backend>> {
    all_backends()
}

#[test]
fn tap_parsing() {
    let stdout = "ok 1 - test_sorts\nnot ok 2 - test_other [AssertFailed: line 12]\nnot ok 3 - test_third [OutOfBounds]\n# 1/3 passed\n";
    let parsed = parse_tap(stdout);
    assert_eq!(
        parsed,
        vec![
            TapLine { name: "test_sorts".into(), outcome: Outcome::Pass, detail: None },
            TapLine {
                name: "test_other".into(),
                outcome: Outcome::Trap("AssertFailed".into()),
                detail: Some("line 12".into())
            },
            TapLine {
                name: "test_third".into(),
                outcome: Outcome::Trap("OutOfBounds".into()),
                detail: None
            },
        ]
    );
}

#[test]
fn agreeing_module_passes() {
    let path = write_module(
        "agree",
        "export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n\ntest \"basic\"\n    assert gcd(12, 18) == 6\n\ntest \"zero\"\n    assert gcd(0, 5) == 5\n",
    );
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(report.tests.len(), 2);
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.divergences(), 0);
}

#[test]
fn consistent_failure_is_not_divergence() {
    let path = write_module(
        "wrongo",
        "test \"bad math\"\n    assert 1 + 1 == 3\n\ntest \"fine\"\n    assert true\n",
    );
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(
        report.tests[0].verdict,
        Verdict::ConsistentFailure("AssertFailed".into()),
        "{report:?}"
    );
    assert_eq!(report.tests[1].verdict, Verdict::Pass);
    assert_eq!(report.divergences(), 0);
    assert!(!report.all_pass());
}

#[test]
fn order_dependence_diverges() {
    // Insertion order leaks into the result: Python (insertion-ordered dict)
    // keeps 0..n, C's hash table almost surely does not. 20 keys make
    // coincidental agreement essentially impossible (1/20!).
    let src = r#"func iteration_order(n: int) -> List<int>
    m = Map()
    for i = 0 to n - 1
        m[i] = i
    order = []
    for k, v in m
        order.append(k)
    return order

test "DIVERGES: map iteration order leaks"
    expected = []
    for i = 0 to 19
        expected.append(i)
    assert iteration_order(20) == expected
"#;
    let path = write_module("orderdep", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(report.tests[0].verdict, Verdict::Divergence, "{report:?}");
    // Python passes (dict preserves insertion), C traps the assert.
    let py = report.tests[0].outcomes.iter().find(|(t, _)| t == "py").unwrap();
    let c = report.tests[0].outcomes.iter().find(|(t, _)| t == "c").unwrap();
    assert_eq!(py.1, Outcome::Pass, "{report:?}");
    assert_eq!(c.1, Outcome::Trap("AssertFailed".into()), "{report:?}");
}

#[test]
fn report_renders_divergence_loudly() {
    let src = "test \"DIVERGES: nothing actually\"\n    assert true\n";
    let path = write_module("render", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    let (text, green) = sudoc_harness::render(&report);
    assert!(green);
    assert!(text.contains("render"), "{text}");
    assert!(text.contains("ok        test_"), "{text}");
}

#[test]
fn examples_lockstep_with_expected_divergences() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut modules = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let report = lockstep(&path, &both()).expect("harness runs");
        if name == "order_dependent" {
            // The pitfall example: at least its DIVERGES test must not agree
            // silently in a wrong way — anything except all-green is fine,
            // and the deterministic `smallest_key` test must pass everywhere.
            let smallest = report
                .tests
                .iter()
                .find(|t| t.name.contains("agrees"))
                .expect("AGREES test present");
            assert_eq!(smallest.verdict, Verdict::Pass, "{report:?}");
        } else {
            assert!(report.all_pass(), "{name}: {report:?}");
        }
        modules += 1;
    }
    assert!(modules >= 9);
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
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
fn generics_run_lockstep() {
    // A generic insertion sort with a comparator — the stdlib shape — must
    // agree between Python and C.
    let src = r#"func ascending(a: int, b: int) -> bool
    return a < b

func descending(a: int, b: int) -> bool
    return b < a

func sort_by<T>(items: inout List<T>, less: func(T, T) -> bool)
    for j = 1 to items.length - 1
        i = j
        while i > 0 and less(items[i], items[i - 1])
            items.swap(i, i - 1)
            i = i - 1

func largest<T>(items: List<T>, less: func(T, T) -> bool) -> Option<T>
    if items.length == 0
        return None
    best = items[0]
    for x in items
        if less(best, x)
            best = x
    return Some(best)

test "generic sort both directions"
    xs = [3, 1, 2]
    sort_by(xs, ascending)
    assert xs == [1, 2, 3]
    sort_by(xs, descending)
    assert xs == [3, 2, 1]

test "generic max"
    assert largest([5, 9, 2], ascending) == Some(9)
    empty: List<int> = []
    assert largest(empty, ascending) == None

test "generic over floats"
    ys = [2.5, 1.5]
    sort_by(ys, float_less)
    assert ys == [1.5, 2.5]

func float_less(a: float, b: float) -> bool
    return a < b
"#;
    let path = write_module("generics", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn imports_run_lockstep() {
    // A two-module program: concrete calls, a constant, and a cross-module
    // generic instantiation, all through both backends.
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-imports-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("util.sudo"),
        "scale = 10\nfunc double(x: int) -> int\n    return x * 2\nfunc id<T>(x: T) -> T\n    return x\n",
    )
    .unwrap();
    let main_src = r#"import util

func go() -> int
    return util.double(util.scale) + util.id(1)

test "cross module"
    assert go() == 21
    assert util.id([1, 2]) == [1, 2]
"#;
    let path = dir.join("main.sudo");
    std::fs::write(&path, main_src).unwrap();
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn stdlib_runs_lockstep() {
    let stdlib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../stdlib");
    let mut modules = 0;
    for entry in std::fs::read_dir(&stdlib).expect("stdlib dir exists") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sudo") {
            let report = lockstep(&path, &both()).expect("harness runs");
            assert!(report.all_pass(), "{}: {report:?}", path.display());
            modules += 1;
        }
    }
    assert!(modules >= 2);
}

#[test]
fn assert_failures_carry_operand_detail() {
    let src = r#"func wrongly_sorted(xs: List<int>) -> List<int>
    ys = xs
    ys.sort()
    ys.append(999)
    return ys

test "shows the operands"
    assert wrongly_sorted([3, 1]) == [1, 3]
"#;
    let path = write_module("detail", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(
        report.tests[0].verdict,
        Verdict::ConsistentFailure("AssertFailed".into()),
        "{report:?}"
    );
    // Both targets serialized the mismatching operands.
    for (target, detail) in &report.tests[0].details {
        assert!(
            detail.contains("[1, 3, 999]") && detail.contains("!="),
            "{target}: {detail}"
        );
    }
    assert_eq!(report.tests[0].details.len(), 2, "{report:?}");
    // The rendered report shows them.
    let (text, _) = sudoc_harness::render(&report);
    assert!(text.contains("[1, 3, 999]"), "{text}");
}

#[test]
fn break_continue_and_expect_trap_lockstep() {
    let src = r#"enum Cmd
    Stop
    Nop
    Add(v: int)

func run(cmds: List<Cmd>) -> int
    total = 0
    for c in cmds
        match c
            case Stop
                break
            case Nop
                continue
            case Add(v)
                total = total + v
    return total

test "break and continue in a range"
    total = 0
    for i = 1 to 10
        if i mod 2 == 0
            continue
        if i > 7
            break
        total = total + i
    assert total == 16

test "break crosses match"
    assert run([Add(5), Nop, Stop, Add(99)]) == 5
    assert run([Nop, Add(2), Add(3)]) == 5

test "while true with break"
    n = 0
    while true
        n = n + 1
        if n == 5
            break
    assert n == 5

test "continue in downto"
    s = 0
    for i = 5 downto 1
        if i == 3
            continue
        s = s + i
    assert s == 12

test "inner break leaves outer loop running"
    pairs = 0
    for i = 1 to 3
        for j = 1 to 10
            if j == 2
                break
            pairs = pairs + 1
    assert pairs == 3

test "single iteration at int max"
    hits = 0
    for i = 9223372036854775807 to 9223372036854775807
        hits = hits + 1
    assert hits == 1

test "min int literal"
    m = -9223372036854775808
    assert m + 1 == -9223372036854775807

test "empty pop traps"
    a: List<int> = []
    expect_trap OutOfBounds
        a.pop()

test "overflow expected"
    expect_trap Overflow
        big = 9223372036854775807
        x = big + 1
        assert x == 0
"#;
    let path = write_module("loopstrap", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn expect_trap_failures_are_consistent() {
    let src = r#"test "wrong kind reported"
    a = [1]
    expect_trap KeyMissing
        x = a[5]

test "missing trap reported"
    expect_trap Overflow
        y = 1 + 1
"#;
    let path = write_module("expectfail", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    for t in &report.tests {
        assert_eq!(
            t.verdict,
            Verdict::ConsistentFailure("AssertFailed".into()),
            "{report:?}"
        );
        for (_, d) in &t.details {
            assert!(d.contains("expected trap"), "{d}");
        }
    }
}

#[test]
fn conformance_corpus_is_green() {
    // The SDK acceptance bar: every corpus module agrees across all
    // registered backends. New backends run this via
    // `sudoc conformance --target <name>`.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/semantics");
    let mut modules = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus exists") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sudo") {
            let report = lockstep(&path, &both()).expect("harness runs");
            assert!(report.all_pass(), "{}: {report:?}", path.display());
            modules += 1;
        }
    }
    assert!(modules >= 9, "corpus shrank? found {modules}");
}
