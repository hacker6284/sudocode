//! In-process totality: a wrapper around `PythonBackend` whose profile is
//! `terminates`. It is not registered in `all_backends()`.
//!
//! The refused body is `while true { break }`. It terminates, so `py` can run
//! it, and the direct rules still refuse it. `while true { skip }` must not be
//! used here: the harness runs with no timeout.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sudoc_backend_py::PythonBackend;
use sudoc_harness::{lockstep_with, render, Backend, HarnessError, Outcome, Verdict};
use sudoc_ir::IrModule;
use sudoc_sdk::{GeneratedFile, Predicate, TestRecipe};

const TERMINATES: &[Predicate] = &[Predicate::Terminates];

/// Shared test plus the refused `while_break`. Both terminate.
const TOTALITY: &str = "\
func while_break()
    while true
        break

func sum_to(n: int) -> int
    s = 0
    for i = 1 to n
        s = s + i
    return s

test \"sums\"
    assert sum_to(3) == 6

test \"breaks immediately\"
    while_break()
";

const REFUSED_EXPORT: &str = "\
export func while_break()
    while true
        break

test \"runs\"
    while_break()
";

struct TotalPython {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Backend for TotalPython {
    fn name(&self) -> &str {
        "totalpy"
    }

    fn profile(&self) -> &'static [Predicate] {
        TERMINATES
    }

    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Result<Vec<GeneratedFile>, String> {
        let mut seen = self.seen.lock().expect("seen");
        for module in modules {
            for func in &module.funcs {
                seen.push(format!("{}.{}", module.name, func.name));
            }
        }
        drop(seen);
        // The wrapper does not filter. If `while_break` is still here, the
        // gate did not run.
        PythonBackend.emit_program(modules, with_tests)
    }

    fn runtime_files(&self) -> Vec<GeneratedFile> {
        PythonBackend.runtime_files()
    }

    fn test_recipe(&self, entry: &str) -> TestRecipe {
        PythonBackend.test_recipe(entry)
    }
}

fn write_module(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-totality-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn totalpy_is_not_a_registered_backend() {
    assert!(sudoc_harness::all_backends()
        .iter()
        .all(|b| b.name() != "totalpy"));
}

#[test]
fn lockstep_skips_while_break_and_agrees_on_the_shared_test() {
    let path = write_module("totality", TOTALITY);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let targets: Vec<Box<dyn Backend>> = vec![
        Box::new(PythonBackend),
        Box::new(TotalPython {
            seen: Arc::clone(&seen),
        }),
    ];
    let report = lockstep_with(&path, &targets, &[]).expect("both tests terminate");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.divergences(), 0);

    let sums = report
        .tests
        .iter()
        .find(|t| t.name == "test_sums")
        .expect("shared test");
    assert_eq!(sums.verdict, Verdict::Pass);
    assert!(sums.outcomes.iter().all(|(_, o)| *o == Outcome::Pass));

    let breaks = report
        .tests
        .iter()
        .find(|t| t.name == "test_breaks_immediately")
        .expect("refused test");
    assert_eq!(breaks.verdict, Verdict::Pass);
    assert_eq!(
        breaks.outcomes.iter().find(|(n, _)| n == "py").unwrap().1,
        Outcome::Pass,
        "py runs the terminating body"
    );
    assert_eq!(
        breaks
            .outcomes
            .iter()
            .find(|(n, _)| n == "totalpy")
            .unwrap()
            .1,
        Outcome::Skipped
    );

    let names = seen.lock().expect("seen");
    assert!(
        names.iter().all(|n| !n.ends_with(".while_break")),
        "recording backend saw while_break: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "totality.sum_to"),
        "expected sum_to to be emitted, saw {names:?}"
    );

    let (text, green) = render(&report);
    assert!(green, "{text}");
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
fn refused_export_is_not_an_emit_error() {
    let path = write_module("exported", REFUSED_EXPORT);
    let targets: Vec<Box<dyn Backend>> = vec![Box::new(TotalPython {
        seen: Arc::new(Mutex::new(Vec::new())),
    })];
    match lockstep_with(&path, &targets, &[]) {
        Err(HarnessError::RefusedExport { target, detail }) => {
            assert_eq!(target, "totalpy");
            assert!(
                detail.contains("while_break"),
                "expected the refused export in the detail, got {detail}"
            );
        }
        other => panic!("expected RefusedExport, got {other:?}"),
    }
}

#[test]
fn empty_profile_still_runs_the_export() {
    let path = write_module("exported_py", REFUSED_EXPORT);
    let targets: Vec<Box<dyn Backend>> = vec![Box::new(PythonBackend)];
    let report = lockstep_with(&path, &targets, &[]).expect("empty profile emits the export");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests[0].outcomes[0].1, Outcome::Pass);
}
