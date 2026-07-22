//! Classification: a target whose test binary crashes with a sanitizer
//! signature on stderr gets a first-class "SANITIZER ... backend bug"
//! detail instead of a silent "runner crashed?" Missing outcome
//! (spec/lockstep.md §5.2).

use std::path::{Path, PathBuf};

use sudoc_backend_ext::ExternalBackend;
use sudoc_harness::{lockstep, Backend, Outcome, Verdict};

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sudoc-sanitize-cls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_module(dir: &Path, name: &str, src: &str) -> PathBuf {
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

/// A fake backend whose emit is trivial and whose run script always prints
/// an AddressSanitizer-signature line to stderr and exits nonzero.
fn fake_asan_backend(dir: &Path) -> ExternalBackend {
    let emit_script = dir.join("emit.py");
    std::fs::write(
        &emit_script,
        r#"
import sys, json
req = json.loads(sys.stdin.read())
print(json.dumps({"files": [{"path": "out.txt", "contents": f"entry={req['entry']}"}]}))
"#,
    )
    .unwrap();
    let emit_abs = emit_script.canonicalize().unwrap();

    let run_script = dir.join("run.py");
    std::fs::write(
        &run_script,
        r#"
import sys
sys.stderr.write("==12345==ERROR: AddressSanitizer: heap-use-after-free on address 0xdeadbeef\n")
sys.exit(1)
"#,
    )
    .unwrap();
    let run_abs = run_script.canonicalize().unwrap();

    let manifest = dir.join("fakeasan.sudoc-backend.json");
    let body = format!(
        r#"{{
  "protocol": 1,
  "name": "fakeasan",
  "emit": ["python3", {:?}],
  "recipe": {{"build": [], "run": ["python3", {:?}]}}
}}"#,
        emit_abs.to_str().unwrap(),
        run_abs.to_str().unwrap(),
    );
    std::fs::write(&manifest, body).unwrap();
    ExternalBackend::load(&manifest).unwrap_or_else(|e| panic!("load fake backend: {e}"))
}

#[test]
fn asan_crash_gets_sanitizer_detail_not_generic_crash_framing() {
    let dir = temp_dir("main");
    let path = write_module(
        &dir,
        "sanitest",
        "test \"one\"\n    assert true\n\ntest \"two\"\n    assert true\n",
    );
    let fake = fake_asan_backend(&dir);
    let targets: Vec<Box<dyn Backend>> = vec![Box::new(fake)];

    let report = lockstep(&path, &targets).expect("harness runs");
    assert_eq!(report.tests.len(), 2);
    for t in &report.tests {
        assert_eq!(t.verdict, Verdict::Divergence, "{t:?}");
        let (target, outcome) = &t.outcomes[0];
        assert_eq!(target, "fakeasan");
        assert_eq!(*outcome, Outcome::Missing, "{t:?}");
        let detail = t
            .details
            .iter()
            .find(|(dt, _)| dt == "fakeasan")
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        assert!(
            detail.starts_with("SANITIZER (this is a sudoc backend bug, please report):"),
            "expected sanitizer-flagged detail, got: {detail:?}"
        );
        assert!(detail.contains("AddressSanitizer"), "{detail}");
    }
    std::fs::remove_dir_all(&dir).ok();
}
