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
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set at runtime"),
    )
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
    assert!(
        out.status.success(),
        "emit-ir failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert!(
        out.status.success(),
        "emit-tests failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<String> =
        serde_json::from_slice(&out.stdout).expect("emit-tests output parses as Vec<String>");
    assert!(!names.is_empty(), "expected at least one test name");

    // Must equal the frontend's own test_fn_names for the same entry module.
    let program = sudoc_types::check_program(&arithmetic()).expect("check arithmetic");
    let entry = program.modules.last().expect("entry module");
    let expected = sudoc_ir::names::test_fn_names(&entry.tests);
    assert_eq!(
        names, expected,
        "emit-tests manifest must match test_fn_names"
    );
}

/// The refused function terminates (`while true { break }`). It is not
/// `while true { skip }`.
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

fn temp_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-emit-skips-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_sudo(dir: &std::path::Path, name: &str, src: &str) -> PathBuf {
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

fn sudoc_output(args: &[std::ffi::OsString]) -> std::process::Output {
    Command::new(sudoc_bin())
        .args(args)
        .output()
        .expect("run sudoc")
}

fn arg(s: &str) -> std::ffi::OsString {
    std::ffi::OsString::from(s)
}

#[test]
fn emit_tests_keeps_a_refused_test_in_the_manifest() {
    let dir = temp_case("manifest");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out = sudoc_output(&[arg("emit-tests"), path.into_os_string()]);
    assert!(
        out.status.success(),
        "emit-tests failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        names,
        vec![
            "test_sums".to_string(),
            "test_breaks_immediately".to_string()
        ]
    );
}

#[test]
fn emit_skips_with_no_profile_writes_empty_arrays() {
    let dir = temp_case("empty");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out_path = dir.join("skips.json");
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert!(
        out.status.success(),
        "emit-skips failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("note:"),
        "empty profile must not print a refusal note"
    );
    let text = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(
        text,
        "{\n  \"predicates\": [],\n  \"skipped_tests\": [],\n  \"refused\": []\n}"
    );
}

#[test]
fn emit_skips_target_py_is_an_empty_profile() {
    let dir = temp_case("py");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out_path = dir.join("skips.json");
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("--target"),
        arg("py"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert!(
        out.status.success(),
        "emit-skips --target py failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(value["predicates"], serde_json::json!([]));
    assert_eq!(value["skipped_tests"], serde_json::json!([]));
    assert_eq!(value["refused"], serde_json::json!([]));
}

#[test]
fn emit_skips_require_terminates_lists_the_refused_test() {
    let dir = temp_case("require");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out_path = dir.join("skips.json");
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("--require"),
        arg("terminates"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert!(
        out.status.success(),
        "emit-skips --require failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("while_break"), "{stderr}");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(value["predicates"], serde_json::json!(["terminates"]));
    assert_eq!(
        value["skipped_tests"],
        serde_json::json!(["test_breaks_immediately"])
    );
    let refused = value["refused"].as_array().unwrap();
    assert_eq!(refused[0]["kind"], "func");
    assert_eq!(refused[0]["name"], "while_break");
    assert_eq!(refused[0]["module"], "totality");
    assert_eq!(refused[0]["predicate"], "terminates");
    assert_eq!(refused[0]["reason"], "while has no decreases measure");
    assert!(refused[0].get("test_fn").is_none());
    assert_eq!(refused[1]["kind"], "test");
    assert_eq!(refused[1]["name"], "breaks immediately");
    assert_eq!(refused[1]["predicate"], "terminates");
    assert_eq!(
        refused[1]["reason"],
        "calls refused function totality.while_break"
    );
    assert_eq!(refused[1]["test_fn"], "test_breaks_immediately");
}

#[test]
fn emit_skips_rejects_target_and_require_together() {
    let dir = temp_case("both");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("--target"),
        arg("py"),
        arg("--require"),
        arg("terminates"),
        path.into_os_string(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("mutually exclusive"));
}

#[test]
fn emit_skips_rejects_an_unknown_predicate() {
    let dir = temp_case("unknown");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out_path = dir.join("skips.json");
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("--require"),
        arg("no_float"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown predicate 'no_float'"));
    assert!(
        !out_path.exists(),
        "unknown predicate must not write the file"
    );
}

#[test]
fn emit_skips_bare_require_is_usage_not_empty_profile() {
    let dir = temp_case("bare");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let out = sudoc_output(&[arg("emit-skips"), path.into_os_string(), arg("--require")]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--require needs a value"));
}

#[test]
fn emit_skips_refused_export_writes_nothing() {
    let dir = temp_case("export");
    let path = write_sudo(&dir, "exported", REFUSED_EXPORT);
    let out_path = dir.join("skips.json");
    let out = sudoc_output(&[
        arg("emit-skips"),
        arg("--require"),
        arg("terminates"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("refused export"));
    assert!(!out_path.exists());
}

#[test]
fn emit_ir_require_strips_before_serializing() {
    let dir = temp_case("ir");
    let path = write_sudo(&dir, "totality", TOTALITY);
    let plain = sudoc_output(&[arg("emit-ir"), path.clone().into_os_string()]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let modules: Vec<sudoc_ir::IrModule> = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(modules.last().unwrap().func("while_break").is_some());

    let gated = sudoc_output(&[
        arg("emit-ir"),
        arg("--require"),
        arg("terminates"),
        path.into_os_string(),
    ]);
    assert!(
        gated.status.success(),
        "{}",
        String::from_utf8_lossy(&gated.stderr)
    );
    let modules: Vec<sudoc_ir::IrModule> = serde_json::from_slice(&gated.stdout).unwrap();
    let entry = modules.last().unwrap();
    assert!(entry.func("while_break").is_none());
    assert!(entry.func("sum_to").is_some());
    assert_eq!(entry.tests.len(), 1);
}

#[test]
fn emit_ir_refused_export_does_not_write() {
    let dir = temp_case("ir-export");
    let path = write_sudo(&dir, "exported", REFUSED_EXPORT);
    let out_path = dir.join("modules.json");
    let out = sudoc_output(&[
        arg("emit-ir"),
        arg("--require"),
        arg("terminates"),
        arg("-o"),
        out_path.clone().into_os_string(),
        path.into_os_string(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!out_path.exists());
}

#[test]
fn protocol_version_stays_4() {
    let out = sudoc_output(&[arg("protocol-version")]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "4");
}
