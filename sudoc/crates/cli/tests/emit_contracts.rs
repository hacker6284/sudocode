//! `sudoc emit-ir` / `emit-tests` contracts: the IR-JSON boundary artifact
//! round-trips back into `Vec<IrModule>`, and the tests manifest lists the
//! entry module's test function names. Runs the built `sudoc` binary so the
//! CLI wiring is exercised end-to-end.

use std::path::PathBuf;
use std::process::Command;

fn sudoc_bin() -> PathBuf {
    // Bazel: SUDOC_BIN set via env=$(rootpath) to the binary in runfiles
    // (resolved relative to the test cwd = runfiles workspace root).
    if let Ok(p) = std::env::var("SUDOC_BIN") {
        let p = PathBuf::from(p);
        return if p.is_absolute() {
            p
        } else {
            std::env::current_dir().expect("cwd").join(p)
        };
    }
    // cargo: CARGO_BIN_EXE_<name> is set at compile time for integration tests.
    // option_env! (matched, not unwrapped) avoids both a hard compile error
    // under Bazel where it is unset and clippy::option_env_unwrap.
    match option_env!("CARGO_BIN_EXE_sudoc") {
        Some(p) => PathBuf::from(p),
        None => panic!("set SUDOC_BIN (Bazel) or run under cargo (CARGO_BIN_EXE_sudoc)"),
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set at runtime"))
}

fn arithmetic() -> PathBuf {
    manifest_dir().join("../../../conformance/semantics/arithmetic.sudo")
}

#[test]
fn emit_ir_roundtrips_to_ir_modules() {
    let out = Command::new(sudoc_bin())
        .arg("emit-ir")
        .arg(arithmetic())
        .output()
        .expect("run sudoc emit-ir");
    assert!(out.status.success(), "emit-ir failed: {}", String::from_utf8_lossy(&out.stderr));
    let modules: Vec<sudoc_ir::IrModule> =
        serde_json::from_slice(&out.stdout).expect("emit-ir output parses as Vec<IrModule>");
    assert!(!modules.is_empty(), "expected at least the entry module");
    // The entry module (last) carries the tests.
    assert!(
        !modules.last().unwrap().tests.is_empty(),
        "arithmetic.sudo has test blocks"
    );
}

#[test]
fn emit_tests_lists_entry_test_names() {
    let out = Command::new(sudoc_bin())
        .arg("emit-tests")
        .arg(arithmetic())
        .output()
        .expect("run sudoc emit-tests");
    assert!(out.status.success(), "emit-tests failed: {}", String::from_utf8_lossy(&out.stderr));
    let names: Vec<String> =
        serde_json::from_slice(&out.stdout).expect("emit-tests output parses as Vec<String>");
    assert!(!names.is_empty(), "expected at least one test name");

    // Must equal the frontend's own test_fn_names for the same entry module.
    let program = sudoc_types::check_program(&arithmetic()).expect("check arithmetic");
    let entry = program.modules.last().expect("entry module");
    let expected = sudoc_ir::names::test_fn_names(&entry.tests);
    assert_eq!(names, expected, "emit-tests manifest must match test_fn_names");
}
