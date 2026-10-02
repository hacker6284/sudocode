//! Two py builds whose sources share module names load side by side in one
//! process, each build directory imported as its own package.

use std::path::{Path, PathBuf};
use std::process::Command;
use sudoc_sdk::Backend;

/// Emit `main` (importing `util`, whose `n()` returns `[n]`) into `dir/pkg`.
fn build(dir: &Path, pkg: &str, n: i64) {
    let (src, out) = (dir.join(format!("{pkg}_src")), dir.join(pkg));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    let util = format!("func n() -> List<int>\n    return [{n}]\n");
    std::fs::write(src.join("util.sudo"), util).unwrap();
    let main = "import util\n\nexport func get() -> List<int>\n    return util.n()\n";
    std::fs::write(src.join("main.sudo"), main).unwrap();
    let p = sudoc_types::check_program(&src.join("main.sudo")).expect("checks");
    let py = sudoc_backend_py::PythonBackend;
    let files = py.emit_program(&p.modules, false).unwrap();
    for f in files.into_iter().chain(py.runtime_files()) {
        std::fs::write(out.join(f.path), f.contents).unwrap();
    }
}

/// Each build answers from its own modules and runtime, not the first build's.
const CHECK: &str = "import sys, one.main, two.main
assert (one.main.get(), two.main.get()) == ([1], [2])
assert one.main._rt is not two.main._rt and one.main.SudoTrap is not two.main.SudoTrap
assert '_sudo_rt' not in sys.modules";

/// Removes the directory even when the test panics.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn two_builds_sharing_module_names_load_in_one_process() {
    let dir =
        TempDir(std::env::temp_dir().join(format!("sudoc-py-packages-{}", std::process::id())));
    build(&dir.0, "one", 1);
    build(&dir.0, "two", 2);
    let out = Command::new("python3")
        .current_dir(&dir.0)
        .args(["-c", CHECK])
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
