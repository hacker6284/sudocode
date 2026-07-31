//! `diff` is the pure core `lockstep_diff` runs. The critical property (design
//! §3 / success criteria): a crashed run leaf that leaves a test unreported must
//! still surface as a divergence — never a silent pass — and a sanitizer-flagged
//! crash must be annotated as such.

use sudoc_harness::{diff, render, CapturedRun, Outcome, Verdict};

fn run(stdout: &str, stderr: &str, exit_code: i32) -> CapturedRun {
    CapturedRun { stdout: stdout.into(), stderr: stderr.into(), exit_code }
}

#[test]
fn all_pass_is_pass() {
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        ("py".to_string(), run("ok 1 - test_a\nok 2 - test_b\n", "", 0)),
        ("js".to_string(), run("ok 1 - test_a\nok 2 - test_b\n", "", 0)),
    ];
    let report = diff("m", &manifest, &runs);
    assert!(report.all_pass());
    assert_eq!(report.divergences(), 0);
}

#[test]
fn crashed_runner_leaves_missing_test_as_divergence() {
    // py passes both; js crashed after test_a (nonzero exit, test_b unreported).
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        ("py".to_string(), run("ok 1 - test_a\nok 2 - test_b\n", "", 0)),
        ("js".to_string(), run("ok 1 - test_a\n", "boom", 1)),
    ];
    let report = diff("m", &manifest, &runs);
    assert!(!report.all_pass(), "a crashed runner must not read as all-pass");
    let tb = report.tests.iter().find(|t| t.name == "test_b").unwrap();
    assert_eq!(tb.verdict, Verdict::Divergence);
    let js = tb.outcomes.iter().find(|(t, _)| t == "js").unwrap();
    assert_eq!(js.1, Outcome::Missing, "unreported test must be Missing, not vanish");
}

#[test]
fn sanitizer_signature_is_annotated_on_missing_tests() {
    // The C-like backend crashed with an AddressSanitizer report (signal death,
    // exit_code -1); its unreported test must carry the SANITIZER detail.
    let manifest = vec!["test_x".to_string()];
    let runs = vec![
        ("py".to_string(), run("ok 1 - test_x\n", "", 0)),
        (
            "c".to_string(),
            run("", "==1==ERROR: AddressSanitizer: heap-buffer-overflow", -1),
        ),
    ];
    let report = diff("m", &manifest, &runs);
    let tx = &report.tests[0];
    assert_eq!(tx.verdict, Verdict::Divergence);
    let detail = tx.details.iter().find(|(t, _)| t == "c").map(|(_, d)| d.as_str());
    assert!(
        detail.is_some_and(|d| d.starts_with("SANITIZER")),
        "sanitizer-flagged crash must be annotated, got {detail:?}"
    );
    // And render() must reproduce the "sanitizer-flagged crash" annotation.
    let (text, green) = render(&report);
    assert!(!green);
    assert!(text.contains("sanitizer-flagged crash"), "render lost the annotation:\n{text}");
}

#[test]
fn same_trap_everywhere_is_consistent_failure() {
    let manifest = vec!["test_t".to_string()];
    let runs = vec![
        ("py".to_string(), run("not ok 1 - test_t [Overflow]\n", "", 1)),
        ("js".to_string(), run("not ok 1 - test_t [Overflow]\n", "", 1)),
    ];
    let report = diff("m", &manifest, &runs);
    assert_eq!(report.tests[0].verdict, Verdict::ConsistentFailure("Overflow".to_string()));
    assert_eq!(report.divergences(), 0);
}
