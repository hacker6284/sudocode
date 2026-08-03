//! Lockstep classification behavior: run real backends and assert the harness
//! correctly labels each outcome — agreement, consistent failure, the flagship
//! divergence, and trap/expect-trap handling.
//!
//! Semantic-agreement coverage (single-module regressions, cross-module and
//! import scenarios, the examples/stdlib/conformance corpora) lives in the
//! decomposed Bazel lockstep: //conformance, //conformance/multimodule,
//! //examples, //stdlib. This suite keeps only what needs the monolithic
//! `lockstep()` — verdict classification — and runs it across py + c, the
//! minimal pair that exhibits agreement, consistent failure, and divergence.

use std::path::PathBuf;

use sudoc_harness::{backend_by_name, lockstep, parse_tap, Backend, Outcome, TapLine, Verdict};

fn write_module(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

/// Python + C: interpreted vs compiled, and — for the order-dependence probe —
/// insertion-ordered dict vs hash table. Enough to exhibit every verdict.
fn both() -> Vec<Box<dyn Backend>> {
    vec![
        backend_by_name("py").expect("py backend"),
        backend_by_name("c").expect("c backend"),
    ]
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
    // Every target serialized the mismatching operands.
    for (target, detail) in &report.tests[0].details {
        assert!(
            detail.contains("[1, 3, 999]") && detail.contains("!="),
            "{target}: {detail}"
        );
    }
    assert_eq!(report.tests[0].details.len(), both().len(), "{report:?}");
    // The rendered report shows them.
    let (text, _) = sudoc_harness::render(&report);
    assert!(text.contains("[1, 3, 999]"), "{text}");
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
