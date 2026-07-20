//! Integration test: discovered backend name colliding with an in-tree backend
//! is a fatal error (fires before conformance reads any directory).

use std::path::PathBuf;
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-cli-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn discovered_name_collides_with_in_tree_py() {
    let dir = temp_dir("discovery-collision");

    let backend_dir = dir.join("backends").join("py");
    std::fs::create_dir_all(&backend_dir).unwrap();

    // Fake emit script: same protocol response shape as backend_ext adapter tests.
    let script = backend_dir.join("emit.py");
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

    // Manifest name "py" deliberately collides with the in-tree Python backend.
    let manifest = backend_dir.join("py.sudoc-backend.json");
    std::fs::write(
        &manifest,
        r#"{
  "protocol": 1,
  "name": "py",
  "emit": ["python3", "emit.py"],
  "recipe": {"build": [], "run": ["true"]}
}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sudoc"))
        .current_dir(&dir)
        .args(["conformance"])
        .output()
        .expect("failed to run sudoc");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected non-zero exit on name collision, status={:?}, stderr={stderr}",
        output.status
    );
    assert!(
        stderr.contains("py"),
        "stderr should mention colliding name 'py', got: {stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
