//! Golden-file tests: every example transpiles to a stable typed-IR dump.
//! Regenerate with `BLESS=1 cargo test -p sudoc-types --test golden`, then
//! review the diff like code — the goldens are the reviewed artifact.

use std::path::{Path, PathBuf};

#[test]
fn golden_ir_dumps() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let golden_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/golden");
    let bless = std::env::var("BLESS").is_ok();
    if bless {
        std::fs::create_dir_all(&golden_dir).unwrap();
    }

    let mut files = walk(&examples);
    files.sort();
    assert!(files.len() >= 9, "expected at least 9 examples");

    let mut mismatches = Vec::new();
    for path in files {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        let ir = sudoc_types::check_source(&src, &name)
            .unwrap_or_else(|e| panic!("{name} failed to check: {}", e[0]));
        let dump = sudoc_ir::pretty::dump(&ir);
        let golden_path = golden_dir.join(format!("{name}.ir"));
        if bless {
            std::fs::write(&golden_path, &dump).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|_| {
            panic!("missing golden {golden_path:?}; run with BLESS=1 to create")
        });
        if dump != expected {
            mismatches.push(name);
        }
    }
    assert!(
        mismatches.is_empty(),
        "IR dumps changed for {mismatches:?} — review, then BLESS=1 to accept"
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("examples dir exists") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "sudo") {
            out.push(path);
        }
    }
    out
}
