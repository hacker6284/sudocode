use std::cell::RefCell;

use sudoc_backend_py::PythonBackend;
use sudoc_ir::names::test_fn_names;
use sudoc_ir::IrModule;
use sudoc_sdk::{Backend, GeneratedFile, TestRecipe};
use sudoc_types::gate::{self, GateError, RefusedKind};
use sudoc_types::termination::{CallKind, CallSite, FuncId, TerminationFacts};
use sudoc_types::{check_program, check_program_files, Program};

fn prog(src: &str) -> Program {
    check_program_files(&[("m", src)]).unwrap_or_else(|es| panic!("check failed: {es:?}"))
}

fn files(pairs: &[(&str, &str)]) -> Program {
    check_program_files(pairs).unwrap_or_else(|es| panic!("check failed: {es:?}"))
}

fn func_names(modules: &[IrModule]) -> Vec<String> {
    modules
        .iter()
        .flat_map(|m| m.funcs.iter().map(|f| format!("{}.{}", m.name, f.name)))
        .collect()
}

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set at runtime"),
    )
}

struct RecordingBackend {
    funcs: RefCell<Vec<String>>,
}

impl Backend for RecordingBackend {
    fn name(&self) -> &str {
        "recording"
    }

    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Result<Vec<GeneratedFile>, String> {
        self.funcs.borrow_mut().extend(func_names(modules));
        PythonBackend.emit_program(modules, with_tests)
    }

    fn runtime_files(&self) -> Vec<GeneratedFile> {
        PythonBackend.runtime_files()
    }

    fn test_recipe(&self, entry: &str) -> TestRecipe {
        PythonBackend.test_recipe(entry)
    }
}

#[test]
fn empty_profile_is_identity() {
    let p = prog(
        "\
func f(n: int) -> int
    return n

test \"a b\"
    assert f(1) == 1
",
    );
    let gated = gate::apply(&p, &[]).expect("empty profile");
    assert_eq!(gated.modules, p.modules);
    assert!(gated.skips.predicates.is_empty());
    assert!(gated.skips.skipped_tests.is_empty());
    assert!(gated.skips.refused.is_empty());
    assert!(gate::refusal_note(&gated.skips).is_none());

    let mut cleared = prog("func f()\n    skip\n");
    let original = cleared.modules.clone();
    cleared.termination = TerminationFacts::default();
    let still = gate::apply(&cleared, &[]).expect("empty profile ignores facts");
    assert_eq!(still.modules, original);
}

#[test]
fn unknown_predicate_is_rejected() {
    let p = prog("func f()\n    skip\n");
    match gate::apply(&p, &["no_float"]) {
        Err(GateError::UnknownPredicate(name)) => {
            assert_eq!(name, "no_float");
            assert_eq!(
                GateError::UnknownPredicate(name).to_string(),
                "unknown predicate 'no_float'"
            );
        }
        other => panic!("expected unknown predicate, got {other:?}"),
    }
    match gate::apply(&p, &["terminates", "no_float"]) {
        Err(GateError::UnknownPredicate(name)) => assert_eq!(name, "no_float"),
        other => panic!("mixed profile must fail closed, got {other:?}"),
    }
}

#[test]
fn duplicate_terminates_is_one_predicate() {
    let p = prog(
        "\
func bad()
    while true
        break
",
    );
    let gated = gate::apply(&p, &["terminates", "terminates"]).expect("known predicate");
    assert_eq!(gated.skips.predicates, vec!["terminates".to_string()]);
    assert!(gated.modules[0].func("bad").is_none());
}

#[test]
fn cone_refuses_callers_and_func_refs() {
    let p = prog(
        "\
record Box
    n: int

N = 1

func bad() -> int
    while true
        break
    return 0

func caller() -> int
    return bad()

func hand() -> func() -> int
    return bad

func use_hand() -> func() -> int
    return hand()

export func kept(b: Box) -> int
    return b.n + N

test \"calls bad\"
    caller()

test \"uses kept\"
    assert kept(Box(2)) == 3
",
    );
    let gated = gate::apply(&p, &["terminates"]).expect("no refused export");
    let names = func_names(&gated.modules);
    assert!(!names.iter().any(|n| n.ends_with(".bad")), "{names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".caller")), "{names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".hand")), "{names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".use_hand")), "{names:?}");
    let kept = gated.modules[0]
        .func("kept")
        .expect("accepted export stays");
    assert!(kept.export);
    assert_eq!(gated.modules[0].records, p.modules[0].records);
    assert_eq!(gated.modules[0].consts, p.modules[0].consts);
    assert_eq!(gated.modules[0].imports, p.modules[0].imports);
    assert_eq!(gated.modules.len(), p.modules.len());

    let caller = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "caller")
        .expect("caller refused");
    assert_eq!(caller.kind, RefusedKind::Func);
    assert_eq!(caller.reason, "calls refused function m.bad");
    assert_eq!(caller.predicate, "terminates");
    let hand = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "hand")
        .expect("func ref refused");
    assert_eq!(hand.reason, "references refused function m.bad");
    let use_hand = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "use_hand")
        .expect("caller of a cone refusal");
    assert_eq!(use_hand.reason, "calls refused function m.hand");

    assert_eq!(
        gated.skips.skipped_tests,
        vec!["test_calls_bad".to_string()]
    );
    assert_eq!(
        test_fn_names(&gated.modules[0].tests),
        vec!["test_uses_kept".to_string()]
    );
    assert!(gated.skips.refused.iter().any(|r| {
        r.kind == RefusedKind::Test
            && r.name == "calls bad"
            && r.test_fn.as_deref() == Some("test_calls_bad")
    }));
    assert_eq!(p.modules[0].tests[1].name, "uses kept");
}

#[test]
fn recorded_call_refuses_even_when_the_body_does_not_call() {
    let mut p = prog(
        "\
func bad()
    while true
        break

func caller() -> int
    return 1
",
    );
    let caller = FuncId {
        module: "m".into(),
        name: "caller".into(),
    };
    p.termination
        .funcs
        .get_mut(&caller)
        .unwrap()
        .calls
        .push(CallSite {
            line: 99,
            col: 3,
            callee: FuncId {
                module: "m".into(),
                name: "bad".into(),
            },
            kind: CallKind::Direct,
        });
    let gated = gate::apply(&p, &["terminates"]).expect("caller is not an export");
    assert!(gated.modules[0].func("caller").is_none());
    assert!(gated.modules[0].func("bad").is_none());
    let item = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "caller")
        .unwrap();
    assert_eq!((item.line, item.col), (99, 3));
    assert_eq!(item.reason, "calls refused function m.bad");
}

#[test]
#[should_panic(expected = "missing termination fact")]
fn missing_termination_fact_is_not_acceptance() {
    let mut p = prog("func f()\n    skip\n");
    p.termination = TerminationFacts::default();
    let _ = gate::apply(&p, &["terminates"]);
}

#[test]
#[should_panic(expected = "missing termination fact")]
fn missing_callee_fact_is_not_acceptance() {
    let mut p = prog("func caller() -> int\n    return 1\n");
    let caller = FuncId {
        module: "m".into(),
        name: "caller".into(),
    };
    p.termination
        .funcs
        .get_mut(&caller)
        .unwrap()
        .calls
        .push(CallSite {
            line: 1,
            col: 1,
            callee: FuncId {
                module: "m".into(),
                name: "missing".into(),
            },
            kind: CallKind::Direct,
        });
    let _ = gate::apply(&p, &["terminates"]);
}

#[test]
fn partition_kept_when_quicksort_range_refused() {
    let p = prog(
        "\
func partition(items: inout List<int>, lo: int, hi: int) -> int
    for j = lo to hi - 1
        if items[j] <= items[hi]
            items.swap(lo, j)
    return lo

func quicksort_range(items: inout List<int>, lo: int, hi: int)
    if lo < hi
        p = partition(items, lo, hi)
        quicksort_range(items, lo, p - 1)

test \"sorts via range\"
    items = [3, 1]
    quicksort_range(items, 0, 1)

test \"partition only\"
    items = [3, 1, 2]
    assert partition(items, 0, 2) >= 0
",
    );
    let original = p.modules[0].func("partition").unwrap().clone();
    let gated = gate::apply(&p, &["terminates"]).expect("neither function is exported");
    assert_eq!(gated.modules[0].func("partition"), Some(&original));
    assert!(gated.modules[0].func("quicksort_range").is_none());
    assert_eq!(
        test_fn_names(&gated.modules[0].tests),
        vec!["test_partition_only".to_string()]
    );
    assert_eq!(
        gated.skips.skipped_tests,
        vec!["test_sorts_via_range".to_string()]
    );
}

#[test]
fn refused_export_fails() {
    let p = prog(
        "\
export func spin()
    while true
        break

export func fine() -> int
    return 1

func helper()
    while true
        break

export func api()
    helper()
",
    );
    let err = gate::apply(&p, &["terminates"]).expect_err("refused exports");
    let GateError::RefusedExport(exports) = err.clone() else {
        panic!("expected RefusedExport, got {err:?}");
    };
    assert_eq!(
        exports.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        vec!["spin", "api"]
    );
    assert_eq!(exports[0].chain.len(), 1);
    assert_eq!(exports[0].chain[0].reason, "while has no decreases measure");
    assert_eq!(exports[1].chain.len(), 2);
    assert_eq!(exports[1].chain[0].name, "api");
    assert_eq!(
        exports[1].chain[0].reason,
        "calls refused function m.helper"
    );
    assert_eq!(exports[1].chain[1].name, "helper");
    assert_eq!(exports[1].chain[1].reason, "while has no decreases measure");
    let text = err.to_string();
    assert!(text.contains("refused export `m.spin`"), "{text}");
    assert!(text.contains("refused export `m.api`"), "{text}");
    assert!(text.contains("calls refused function m.helper"), "{text}");
    assert!(!text.contains("`fine`"), "{text}");
}

#[test]
fn refused_dependency_export_fails() {
    let p = files(&[
        (
            "helper",
            "\
export func ok() -> int
    return 1

export func bad()
    while true
        break
",
        ),
        (
            "app",
            "\
import helper

func main() -> int
    return helper.ok()
",
        ),
    ]);
    let err = gate::apply(&p, &["terminates"]).expect_err("dep export");
    let GateError::RefusedExport(exports) = err else {
        panic!("expected RefusedExport");
    };
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].module, "helper");
    assert_eq!(exports[0].name, "bad");
    assert_eq!(exports[0].chain[0].reason, "while has no decreases measure");
}

#[test]
fn cross_module_caller_and_dependency_test_are_refused() {
    let p = files(&[
        (
            "helper",
            "\
func bad()
    while true
        break

export func ok() -> int
    return 1

test \"dep touches bad\"
    bad()

test \"dep ok\"
    assert ok() == 1
",
        ),
        (
            "app",
            "\
import helper

func calls_bad()
    helper.bad()

func hand() -> func()
    return helper.bad

func calls_ok() -> int
    return helper.ok()

test \"entry ok\"
    assert calls_ok() == 1
",
        ),
    ]);
    let gated = gate::apply(&p, &["terminates"]).expect("bad is not exported");
    assert_eq!(
        gated
            .modules
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        vec!["helper", "app"]
    );
    assert!(gated.modules[0].func("bad").is_none());
    assert!(gated.modules[0].func("ok").unwrap().export);
    assert!(gated.modules[1].func("calls_bad").is_none());
    assert!(gated.modules[1].func("hand").is_none());
    assert!(gated.modules[1].func("calls_ok").is_some());
    assert_eq!(gated.modules[0].tests.len(), 1);
    assert_eq!(gated.modules[0].tests[0].name, "dep ok");
    let hand = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "hand")
        .expect("cross-module func ref");
    assert_eq!(hand.reason, "references refused function helper.bad");
    let calls_bad = gated
        .skips
        .refused
        .iter()
        .find(|r| r.name == "calls_bad")
        .unwrap();
    assert_eq!(calls_bad.reason, "calls refused function helper.bad");
    assert!(gated.skips.skipped_tests.is_empty());
    assert!(gated
        .skips
        .refused
        .iter()
        .any(|r| r.kind == RefusedKind::Test
            && r.name == "dep touches bad"
            && r.test_fn.is_none()));
}

#[test]
fn survivor_test_name_stable_after_colliding_predecessor_stripped() {
    let p = prog(
        "\
func bad()
    while true
        break

test \"a b\"
    bad()

test \"a_b\"
    skip
",
    );
    let full = test_fn_names(&p.modules[0].tests);
    assert_eq!(full, vec!["test_a_b".to_string(), "test_a_b_2".to_string()]);
    let gated = gate::apply(&p, &["terminates"]).unwrap();
    assert_eq!(
        test_fn_names(&gated.modules[0].tests),
        vec![full[1].clone()]
    );
    assert_eq!(gated.modules[0].tests[0].name, "a_b_2");
    assert_eq!(gated.skips.skipped_tests, vec!["test_a_b".to_string()]);
    assert_eq!(p.modules[0].tests[0].name, "a b");
    assert_eq!(p.modules[0].tests[1].name, "a_b");

    let empty = gate::apply(&p, &[]).unwrap();
    assert_eq!(empty.modules[0].tests[0].name, "a b");
    assert_eq!(empty.modules[0].tests[1].name, "a_b");
}

#[test]
fn while_break_absent_for_terminates_and_present_for_empty_profile() {
    let src = "\
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
    let p = prog(src);
    let empty = gate::apply(&p, &[]).unwrap();
    assert!(empty.modules[0].func("while_break").is_some());
    assert_eq!(empty.modules, p.modules);

    let gated = gate::apply(&p, &["terminates"]).unwrap();
    assert!(gated.modules[0].func("while_break").is_none());
    assert!(gated.modules[0].func("sum_to").is_some());
    assert_eq!(
        gated.skips.skipped_tests,
        vec!["test_breaks_immediately".to_string()]
    );
    assert_eq!(
        test_fn_names(&gated.modules[0].tests),
        vec!["test_sums".to_string()]
    );
    let note = gate::refusal_note(&gated.skips).expect("strip note");
    assert!(
        note.starts_with("note: predicate `terminates` refused "),
        "{note}"
    );
    assert!(note.contains("`while_break`"), "{note}");
    assert!(note.contains("while has no decreases measure"), "{note}");
    assert!(note.contains("test \"breaks immediately\""), "{note}");
    assert!(
        note.contains("calls refused function m.while_break"),
        "{note}"
    );

    let recording = RecordingBackend {
        funcs: RefCell::new(Vec::new()),
    };
    let emitted = recording
        .emit_program(&gated.modules, true)
        .expect("emitter does not filter");
    assert!(recording
        .funcs
        .borrow()
        .iter()
        .all(|n| !n.ends_with(".while_break")));
    assert!(emitted
        .iter()
        .all(|f| !f.contents.contains("def while_break")));

    let raw = PythonBackend
        .emit_program(&empty.modules, true)
        .expect("empty profile");
    assert!(raw.iter().any(|f| f.contents.contains("def while_break")));
    assert!(PythonBackend.profile().is_empty());
}

#[test]
fn empty_profile_emits_gcd_unchanged() {
    let path = manifest_dir().join("../../../examples/gcd.sudo");
    let program = check_program(&path).unwrap_or_else(|es| panic!("gcd: {es:?}"));
    let gated = gate::apply(&program, &[]).expect("empty profile");
    assert_eq!(gated.modules, program.modules);
    assert!(PythonBackend.profile().is_empty());
    let direct = PythonBackend
        .emit_program(&program.modules, true)
        .expect("emit gcd");
    let via_gate = PythonBackend
        .emit_program(&gated.modules, true)
        .expect("emit gated gcd");
    assert_eq!(direct.len(), via_gate.len());
    for (a, b) in direct.iter().zip(&via_gate) {
        assert_eq!(a.path, b.path);
        assert_eq!(a.contents, b.contents);
    }
    assert!(
        via_gate.iter().any(|f| f.contents.contains("def gcd(")),
        "empty profile still emits gcd"
    );
    let err = gate::apply(&program, &["terminates"]).expect_err("gcd is a refused export");
    let GateError::RefusedExport(exports) = err else {
        panic!("expected RefusedExport");
    };
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "gcd");
    assert_eq!(exports[0].chain[0].reason, "while has no decreases measure");
}

#[test]
fn cone_chain_stops_at_eight_frames() {
    let mut src = String::from("func f0()\n    while true\n        break\n");
    for i in 1..10 {
        if i == 9 {
            src.push_str(&format!("export func f{i}()\n    f{}()\n", i - 1));
        } else {
            src.push_str(&format!("func f{i}()\n    f{}()\n", i - 1));
        }
    }
    let p = prog(&src);
    let err = gate::apply(&p, &["terminates"]).expect_err("f9 is a refused export");
    let GateError::RefusedExport(exports) = err else {
        panic!("expected RefusedExport");
    };
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "f9");
    assert_eq!(exports[0].chain.len(), 8);
    assert_eq!(exports[0].chain[0].name, "f9");
    assert!(
        exports[0].chain[0].reason.contains("m.f8"),
        "{:?}",
        exports[0].chain[0]
    );
    assert_eq!(exports[0].chain[7].name, "f2");
    assert!(
        exports[0].chain[7].reason.contains("m.f1"),
        "{:?}",
        exports[0].chain[7]
    );
    assert!(exports[0]
        .chain
        .iter()
        .all(|frame| frame.reason != "while has no decreases measure"));
}
