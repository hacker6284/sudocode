//! Integration tests for ExternalBackend: load, emit protocol, path checks.

use std::path::{Path, PathBuf};

use sudoc_backend_ext::ExternalBackend;
use sudoc_sdk::Backend;

const SRC: &str = r#"
func add(a: int, b: int) -> int
    return a + b

test "add works"
    assert add(1, 2) == 3
"#;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-ext-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_manifest(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn sample_modules() -> Vec<sudoc_ir::IrModule> {
    let m = sudoc_types::check_source(SRC, "add").expect("checks");
    vec![m]
}

fn make_backend(dir: &Path, name: &str, emit: &[&str], recipe: &str) -> ExternalBackend {
    let emit_json: Vec<String> = emit.iter().map(|s| s.to_string()).collect();
    let emit_arr = serde_json::to_string(&emit_json).unwrap();
    let manifest = format!(
        r#"{{
  "protocol": 1,
  "name": "{name}",
  "emit": {emit_arr},
  "recipe": {recipe}
}}"#
    );
    let path = write_manifest(dir, &format!("{name}.json"), &manifest);
    ExternalBackend::load(&path).unwrap_or_else(|e| panic!("load {name}: {e}"))
}

#[test]
fn happy_path_transmits_entry_and_with_tests() {
    let dir = temp_dir("happy");
    let script = dir.join("emit.py");
    std::fs::write(
        &script,
        r#"
import sys, json
req = json.loads(sys.stdin.read())
assert req["protocol"] == 1
assert req["cmd"] == "emit"
out = {
    "files": [{
        "path": "out.txt",
        "contents": f"entry={req['entry']} with_tests={json.dumps(req['with_tests'])}"
    }]
}
print(json.dumps(out))
"#,
    )
    .unwrap();
    let script_abs = script.canonicalize().unwrap();
    let backend = make_backend(
        &dir,
        "fake",
        &["python3", script_abs.to_str().unwrap()],
        r#"{"build": [], "run": ["true"]}"#,
    );
    let modules = sample_modules();
    let files = backend
        .emit_program(&modules, true)
        .unwrap_or_else(|e| panic!("emit: {e}"));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "out.txt");
    assert_eq!(files[0].contents, "entry=add with_tests=true");

    let files_no = backend
        .emit_program(&modules, false)
        .unwrap_or_else(|e| panic!("emit: {e}"));
    assert_eq!(files_no[0].contents, "entry=add with_tests=false");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn error_response_surfaces_message() {
    let dir = temp_dir("err-resp");
    let script = dir.join("emit.py");
    std::fs::write(
        &script,
        r#"
import sys, json
print(json.dumps({"error": "boom"}))
"#,
    )
    .unwrap();
    let script_abs = script.canonicalize().unwrap();
    let backend = make_backend(
        &dir,
        "errfake",
        &["python3", script_abs.to_str().unwrap()],
        r#"{"build": [], "run": ["true"]}"#,
    );
    let modules = sample_modules();
    let err = backend.emit_program(&modules, true).unwrap_err();
    assert!(err.contains("boom"), "err={err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn nonzero_exit_includes_stderr() {
    let dir = temp_dir("nonzero");
    let script = dir.join("emit.py");
    std::fs::write(
        &script,
        r#"
import sys
sys.stderr.write("kaboom-detail\n")
sys.exit(1)
"#,
    )
    .unwrap();
    let script_abs = script.canonicalize().unwrap();
    let backend = make_backend(
        &dir,
        "boomfake",
        &["python3", script_abs.to_str().unwrap()],
        r#"{"build": [], "run": ["true"]}"#,
    );
    let modules = sample_modules();
    let err = backend.emit_program(&modules, true).unwrap_err();
    assert!(err.contains("kaboom-detail"), "err={err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn path_traversal_rejected() {
    let dir = temp_dir("traverse");
    let script = dir.join("emit.py");
    std::fs::write(
        &script,
        r#"
import sys, json
print(json.dumps({"files": [{"path": "../evil", "contents": "x"}]}))
"#,
    )
    .unwrap();
    let script_abs = script.canonicalize().unwrap();
    let backend = make_backend(
        &dir,
        "evilfake",
        &["python3", script_abs.to_str().unwrap()],
        r#"{"build": [], "run": ["true"]}"#,
    );
    let modules = sample_modules();
    let err = backend.emit_program(&modules, true).unwrap_err();
    assert!(
        err.contains("..") || err.contains("evil"),
        "err={err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn entry_substitution_in_recipe() {
    let dir = temp_dir("recipe");
    let path = write_manifest(
        &dir,
        "sub.json",
        r#"{
  "protocol": 1,
  "name": "subfake",
  "emit": ["true"],
  "recipe": {
    "build": [["build", "{entry}_test"]],
    "run": ["run", "{entry}_test"]
  }
}"#,
    );
    let backend = ExternalBackend::load(&path).unwrap();
    let recipe = backend.test_recipe("foo");
    assert_eq!(
        recipe.build,
        vec![vec!["build".to_string(), "foo_test".to_string()]]
    );
    assert_eq!(recipe.run, vec!["run".to_string(), "foo_test".to_string()]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn protocol_rejection() {
    let dir = temp_dir("proto");
    let path = write_manifest(
        &dir,
        "bad.json",
        r#"{
  "protocol": 2,
  "name": "bad",
  "emit": ["true"],
  "recipe": {"build": [], "run": ["true"]}
}"#,
    );
    let err = match ExternalBackend::load(&path) {
        Ok(_) => panic!("expected protocol rejection"),
        Err(e) => e,
    };
    assert!(
        err.contains("protocol") || err.contains("2"),
        "err={err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn emit_child_cwd_is_manifest_dir() {
    // Prove emit runs with the manifest directory as cwd: the script is
    // referenced by bare relative name (argv[1]), so the child must chdir to
    // the tempdir. The test process cwd is the crate root (cargo test), not
    // the tempdir — no global set_current_dir needed (and not used: racy).
    let dir = temp_dir("cwd");
    let script = dir.join("fake_backend.py");
    std::fs::write(
        &script,
        r#"
import sys, json
req = json.loads(sys.stdin.read())
assert req["protocol"] == 1
assert req["cmd"] == "emit"
out = {
    "files": [{
        "path": "out.txt",
        "contents": f"cwd-ok entry={req['entry']}"
    }]
}
print(json.dumps(out))
"#,
    )
    .unwrap();
    let path = write_manifest(
        &dir,
        "cwd.json",
        r#"{
  "protocol": 1,
  "name": "cwdfake",
  "emit": ["python3", "fake_backend.py"],
  "recipe": {"build": [], "run": ["true"]}
}"#,
    );
    // Absolute path so load is not cwd-dependent; ambient process cwd stays
    // away from the tempdir (cargo test cwd is the crate root).
    let path = path.canonicalize().unwrap();
    let backend = ExternalBackend::load(&path).unwrap_or_else(|e| panic!("load: {e}"));
    let modules = sample_modules();
    let files = backend
        .emit_program(&modules, true)
        .unwrap_or_else(|e| panic!("emit: {e}"));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "out.txt");
    assert_eq!(files[0].contents, "cwd-ok entry=add");
    std::fs::remove_dir_all(&dir).ok();
}
