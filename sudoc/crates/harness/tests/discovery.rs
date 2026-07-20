//! Discovery of `backends/*/*.sudoc-backend.json` under a project root.

use std::path::{Path, PathBuf};

use sudoc_harness::discovered_backends;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sudoc-discovery-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_fake_backend(root: &Path, subdir: &str, name: &str) -> PathBuf {
    let backend_dir = root.join("backends").join(subdir);
    std::fs::create_dir_all(&backend_dir).unwrap();
    let script = backend_dir.join("emit.py");
    std::fs::write(
        &script,
        r#"
import sys, json
req = json.loads(sys.stdin.read())
print(json.dumps({
    "files": [{"path": "out.txt", "contents": f"entry={req['entry']}"}]
}))
"#,
    )
    .unwrap();
    let script_abs = script.canonicalize().unwrap();
    let manifest_path = backend_dir.join(format!("{name}.sudoc-backend.json"));
    let emit = format!("[\"python3\", {:?}]", script_abs.to_str().unwrap());
    let body = format!(
        r#"{{
  "protocol": 1,
  "name": "{name}",
  "emit": {emit},
  "recipe": {{"build": [], "run": ["true"]}}
}}"#
    );
    std::fs::write(&manifest_path, body).unwrap();
    manifest_path
}

#[test]
fn discovers_backend_under_backends_subdir() {
    let root = temp_dir("happy");
    write_fake_backend(&root, "foo", "foo");
    let found = discovered_backends(&root).expect("discover");
    let names: Vec<&str> = found.iter().map(|b| b.name()).collect();
    assert!(
        names.contains(&"foo"),
        "expected foo in discovered names: {names:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn missing_backends_dir_is_empty_ok() {
    let root = temp_dir("no-backends");
    // no backends/ at all
    let found = discovered_backends(&root).expect("discover");
    assert!(found.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn malformed_manifest_is_hard_error_with_path() {
    let root = temp_dir("malformed");
    let backend_dir = root.join("backends").join("bad");
    std::fs::create_dir_all(&backend_dir).unwrap();
    let manifest_path = backend_dir.join("bad.sudoc-backend.json");
    std::fs::write(&manifest_path, "{ not valid json").unwrap();
    // Canonicalize for a stable path prefix to match against (load uses the path as given).
    let err = match discovered_backends(&root) {
        Ok(_) => panic!("malformed must error"),
        Err(e) => e,
    };
    let path_str = manifest_path.display().to_string();
    assert!(
        err.contains(&path_str) || err.contains("bad.sudoc-backend.json"),
        "error must mention manifest path; got: {err}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn ignores_manifests_directly_under_backends() {
    let root = temp_dir("flat");
    let backends = root.join("backends");
    std::fs::create_dir_all(&backends).unwrap();
    // File directly under backends/ — must be ignored (only backends/*/).
    std::fs::write(
        backends.join("orphan.sudoc-backend.json"),
        r#"{
  "protocol": 1,
  "name": "orphan",
  "emit": ["true"],
  "recipe": {"build": [], "run": ["true"]}
}"#,
    )
    .unwrap();
    let found = discovered_backends(&root).expect("discover");
    assert!(
        found.is_empty(),
        "flat manifests under backends/ must be ignored, got {} backends",
        found.len()
    );
    std::fs::remove_dir_all(&root).ok();
}
