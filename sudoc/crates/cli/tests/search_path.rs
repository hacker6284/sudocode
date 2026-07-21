use std::path::PathBuf;
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-cli-searchpath-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn dash_i_resolves_plain_import() {
    let entry_dir = temp_dir("entry");
    let dep_dir = temp_dir("dep");
    std::fs::write(dep_dir.join("util.sudo"), "func triple(x: int) -> int\n    return x * 3\n").unwrap();
    std::fs::write(
        entry_dir.join("main.sudo"),
        "import util\n\nfunc f() -> int\n    return util.triple(2)\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sudoc"))
        .args(["check", "-I", dep_dir.to_str().unwrap(), entry_dir.join("main.sudo").to_str().unwrap()])
        .output()
        .expect("failed to run sudoc");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(&entry_dir).ok();
    std::fs::remove_dir_all(&dep_dir).ok();
}

#[test]
fn check_without_dash_i_fails_to_find_the_module() {
    let entry_dir = temp_dir("nodashi-entry");
    let dep_dir = temp_dir("nodashi-dep");
    std::fs::write(dep_dir.join("util.sudo"), "func triple(x: int) -> int\n    return x * 3\n").unwrap();
    std::fs::write(
        entry_dir.join("main.sudo"),
        "import util\n\nfunc f() -> int\n    return util.triple(2)\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sudoc"))
        .args(["check", entry_dir.join("main.sudo").to_str().unwrap()])
        .output()
        .expect("failed to run sudoc");
    assert!(!output.status.success());

    std::fs::remove_dir_all(&entry_dir).ok();
    std::fs::remove_dir_all(&dep_dir).ok();
}
