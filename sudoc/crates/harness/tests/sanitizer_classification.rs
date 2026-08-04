//! Classification: a target whose test binary crashes with a sanitizer
//! signature on stderr gets a first-class "SANITIZER ... backend bug"
//! detail instead of a silent "runner crashed?" Missing outcome
//! (spec/lockstep.md §5.2).
//!
//! Phase 5: rewritten off `sudoc_backend_ext::ExternalBackend` (a fake
//! manifest-driven external backend) onto an in-test `Backend` impl. The
//! external-backend *registration/discovery* path was retired with `--external`
//! / `discovered_backends()` (spec §2.4); the surviving emit/recipe boundary is
//! exercised by the Bazel `sudo_external_backend` rule (hs) end-to-end. This
//! unit test only needs *a* backend whose run emits a sanitizer signature, so it
//! synthesizes one directly — no crate dependency, no spawned interpreter beyond
//! the `sh` the recipe already runs.

use std::path::{Path, PathBuf};

use sudoc_harness::{lockstep, Backend, Outcome, Verdict};
use sudoc_ir::IrModule;
use sudoc_sdk::{GeneratedFile, TestRecipe};

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

/// A backend whose emit is trivial and whose run command always prints an
/// AddressSanitizer-signature line to stderr and exits nonzero — the shape a
/// real C/Rust backend takes when its instrumented binary traps on a memory
/// bug. It produces no outcome-protocol stdout, so every test is Missing and
/// must be classified as a sanitizer crash (backend bug), not a silent crash.
struct FakeAsanBackend;

impl Backend for FakeAsanBackend {
    fn name(&self) -> &str {
        "fakeasan"
    }

    fn emit_program(
        &self,
        _modules: &[IrModule],
        _with_tests: bool,
    ) -> Result<Vec<GeneratedFile>, String> {
        Ok(vec![GeneratedFile {
            path: "out.txt".to_string(),
            contents: "fakeasan\n".to_string(),
        }])
    }

    fn runtime_files(&self) -> Vec<GeneratedFile> {
        Vec::new()
    }

    fn test_recipe(&self, _entry: &str) -> TestRecipe {
        TestRecipe {
            build: Vec::new(),
            run: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '==12345==ERROR: AddressSanitizer: heap-use-after-free on address 0xdeadbeef\\n' >&2; exit 1"
                    .to_string(),
            ],
        }
    }
}

#[test]
fn asan_crash_gets_sanitizer_detail_not_generic_crash_framing() {
    let dir = temp_dir("main");
    let path = write_module(
        &dir,
        "sanitest",
        "test \"one\"\n    assert true\n\ntest \"two\"\n    assert true\n",
    );
    let targets: Vec<Box<dyn Backend>> = vec![Box::new(FakeAsanBackend)];

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
