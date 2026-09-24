//! `diff` is the pure core `lockstep_diff` runs. The critical property (design
//! §3 / success criteria): a crashed run leaf that leaves a test unreported must
//! still surface as a divergence — never a silent pass — and a sanitizer-flagged
//! crash must be annotated as such.

use std::collections::{BTreeMap, BTreeSet};

use sudoc_harness::{diff, render, CapturedRun, Outcome, Verdict};

fn skips_of(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    pairs
        .iter()
        .map(|(backend, tests)| {
            (
                (*backend).to_string(),
                tests.iter().map(|t| (*t).to_string()).collect(),
            )
        })
        .collect()
}

fn run(stdout: &str, stderr: &str, exit_code: i32) -> CapturedRun {
    CapturedRun {
        stdout: stdout.into(),
        stderr: stderr.into(),
        exit_code,
    }
}

#[test]
fn all_pass_is_pass() {
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_a\nok 2 - test_b\n", "", 0),
        ),
        (
            "js".to_string(),
            run("ok 1 - test_a\nok 2 - test_b\n", "", 0),
        ),
    ];
    let report = diff("m", &manifest, &runs, &BTreeMap::new());
    assert!(report.all_pass());
    assert_eq!(report.divergences(), 0);
}

#[test]
fn crashed_runner_leaves_missing_test_as_divergence() {
    // py passes both; js crashed after test_a (nonzero exit, test_b unreported).
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_a\nok 2 - test_b\n", "", 0),
        ),
        ("js".to_string(), run("ok 1 - test_a\n", "boom", 1)),
    ];
    let report = diff("m", &manifest, &runs, &BTreeMap::new());
    assert!(
        !report.all_pass(),
        "a crashed runner must not read as all-pass"
    );
    let tb = report.tests.iter().find(|t| t.name == "test_b").unwrap();
    assert_eq!(tb.verdict, Verdict::Divergence);
    let js = tb.outcomes.iter().find(|(t, _)| t == "js").unwrap();
    assert_eq!(
        js.1,
        Outcome::Missing,
        "unreported test must be Missing, not vanish"
    );
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
    let report = diff("m", &manifest, &runs, &BTreeMap::new());
    let tx = &report.tests[0];
    assert_eq!(tx.verdict, Verdict::Divergence);
    let detail = tx
        .details
        .iter()
        .find(|(t, _)| t == "c")
        .map(|(_, d)| d.as_str());
    assert!(
        detail.is_some_and(|d| d.starts_with("SANITIZER")),
        "sanitizer-flagged crash must be annotated, got {detail:?}"
    );
    // And render() must reproduce the "sanitizer-flagged crash" annotation.
    let (text, green) = render(&report);
    assert!(!green);
    assert!(
        text.contains("sanitizer-flagged crash"),
        "render lost the annotation:\n{text}"
    );
}

#[test]
fn same_trap_everywhere_is_consistent_failure() {
    let manifest = vec!["test_t".to_string()];
    let runs = vec![
        (
            "py".to_string(),
            run("not ok 1 - test_t [Overflow]\n", "", 1),
        ),
        (
            "js".to_string(),
            run("not ok 1 - test_t [Overflow]\n", "", 1),
        ),
    ];
    let report = diff("m", &manifest, &runs, &BTreeMap::new());
    assert_eq!(
        report.tests[0].verdict,
        Verdict::ConsistentFailure("Overflow".to_string())
    );
    assert_eq!(report.divergences(), 0);
}

#[test]
fn skip_is_not_a_divergence() {
    let manifest = vec![
        "test_sums".to_string(),
        "test_breaks_immediately".to_string(),
    ];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_sums\nok 2 - test_breaks_immediately\n", "", 0),
        ),
        ("totalpy".to_string(), run("ok 1 - test_sums\n", "", 0)),
    ];
    let skips = skips_of(&[("totalpy", &["test_breaks_immediately"])]);
    let report = diff("totality", &manifest, &runs, &skips);
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.divergences(), 0);
    let skipped = report
        .tests
        .iter()
        .find(|t| t.name == "test_breaks_immediately")
        .unwrap();
    assert_eq!(skipped.verdict, Verdict::Pass);
    assert_eq!(
        skipped
            .outcomes
            .iter()
            .find(|(t, _)| t == "totalpy")
            .map(|(_, o)| o),
        Some(&Outcome::Skipped)
    );
}

#[test]
fn missing_tap_on_a_participant_is_still_divergence() {
    // totalpy skipped test_b. js did not, and printed nothing for it.
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_a\nok 2 - test_b\n", "", 0),
        ),
        ("totalpy".to_string(), run("ok 1 - test_a\n", "", 0)),
        ("js".to_string(), run("ok 1 - test_a\n", "boom", 1)),
    ];
    let skips = skips_of(&[("totalpy", &["test_b"])]);
    let report = diff("m", &manifest, &runs, &skips);
    assert!(!report.all_pass());
    let tb = report.tests.iter().find(|t| t.name == "test_b").unwrap();
    assert_eq!(tb.verdict, Verdict::Divergence);
    assert_eq!(
        tb.outcomes.iter().find(|(t, _)| t == "js").unwrap().1,
        Outcome::Missing
    );
    assert_eq!(
        tb.outcomes.iter().find(|(t, _)| t == "totalpy").unwrap().1,
        Outcome::Skipped
    );
}

#[test]
fn tap_for_a_skipped_name_is_filter_bug() {
    let manifest = vec!["test_breaks_immediately".to_string()];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_breaks_immediately\n", "", 0),
        ),
        (
            "totalpy".to_string(),
            run("ok 1 - test_breaks_immediately\n", "", 0),
        ),
    ];
    let skips = skips_of(&[("totalpy", &["test_breaks_immediately"])]);
    let report = diff("totality", &manifest, &runs, &skips);
    assert_eq!(report.tests[0].verdict, Verdict::FilterBug);
    assert!(!report.all_pass());
    let (text, green) = render(&report);
    assert!(!green);
    assert!(text.contains("FILTER BUG"), "{text}");
    assert!(text.contains("skip"), "{text}");
}

#[test]
fn all_skipped_module_fails() {
    let manifest = vec!["test_a".to_string(), "test_b".to_string()];
    let runs = vec![
        ("totalpy".to_string(), run("", "", 0)),
        ("other".to_string(), run("", "", 0)),
    ];
    let skips = skips_of(&[
        ("totalpy", &["test_a", "test_b"]),
        ("other", &["test_a", "test_b"]),
    ]);
    let report = diff("totality", &manifest, &runs, &skips);
    assert!(report
        .tests
        .iter()
        .all(|t| t.verdict == Verdict::AllSkipped));
    assert!(!report.all_pass());
    let (text, green) = render(&report);
    assert!(!green);
    assert!(
        text.contains("module 'totality' was refused by every backend"),
        "{text}"
    );
}

#[test]
fn stack_overflow_versus_pass_is_still_a_divergence() {
    let manifest = vec!["test_rec".to_string()];
    let runs = vec![
        ("py".to_string(), run("ok 1 - test_rec\n", "", 0)),
        (
            "c".to_string(),
            run("not ok 1 - test_rec [StackOverflow]\n", "", 1),
        ),
    ];
    // A skip of some other name must not swallow the hint.
    let skips = skips_of(&[("c", &["test_other"])]);
    let report = diff("m", &manifest, &runs, &skips);
    assert_eq!(report.tests[0].verdict, Verdict::Divergence);
    assert!(!report.all_pass());
    let (text, green) = render(&report);
    assert!(!green);
    assert!(
        text.contains(
            "note: a StackOverflow divergence usually means recursion depth exceeded one\n   \
             target's stack — not necessarily a logic bug."
        ),
        "{text}"
    );
}

#[test]
fn pass_with_skip_renders_the_sample() {
    let manifest = vec![
        "test_sums".to_string(),
        "test_breaks_immediately".to_string(),
    ];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_sums\nok 2 - test_breaks_immediately\n", "", 0),
        ),
        ("totalpy".to_string(), run("ok 1 - test_sums\n", "", 0)),
    ];
    let skips = skips_of(&[("totalpy", &["test_breaks_immediately"])]);
    let report = diff("totality", &manifest, &runs, &skips);
    let (text, green) = render(&report);
    assert!(green);
    assert_eq!(
        text,
        "\
== totality (2 tests; targets: py, totalpy)
   ok        test_sums
   ok        test_breaks_immediately
                totalpy  skip
"
    );
}

#[test]
fn skip_on_one_test_does_not_waive_a_disagreement() {
    let manifest = vec![
        "test_sums".to_string(),
        "test_breaks_immediately".to_string(),
    ];
    let runs = vec![
        (
            "py".to_string(),
            run("ok 1 - test_sums\nok 2 - test_breaks_immediately\n", "", 0),
        ),
        (
            "totalpy".to_string(),
            run("not ok 1 - test_sums [AssertFailed]\n", "", 1),
        ),
    ];
    let skips = skips_of(&[("totalpy", &["test_breaks_immediately"])]);
    let report = diff("totality", &manifest, &runs, &skips);
    assert_eq!(report.tests[0].verdict, Verdict::Divergence);
    assert_eq!(report.tests[1].verdict, Verdict::Pass);
    assert!(!report.all_pass());
    let (text, green) = render(&report);
    assert!(!green);
    assert!(text.contains("DIVERGED  test_sums"), "{text}");
    assert!(text.contains("totalpy  skip"), "{text}");
}
