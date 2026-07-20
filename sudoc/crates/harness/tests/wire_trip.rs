//! Wire-trip: backend emission through serialize→deserialize must match direct.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sudoc_sdk::GeneratedFile;

fn files_map(files: Vec<GeneratedFile>) -> BTreeMap<String, String> {
    files.into_iter().map(|f| (f.path, f.contents)).collect()
}

fn semantics_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/semantics");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("conformance/semantics exists") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sudo") {
            out.push(path);
        }
    }
    out.sort();
    assert!(!out.is_empty(), "expected semantics .sudo files");
    out
}

#[test]
fn wire_trip_emission_matches_direct() {
    let backends = sudoc_harness::all_backends();
    for path in semantics_files() {
        let program = sudoc_types::check_program(&path).unwrap_or_else(|e| {
            panic!("{} failed to check: {}", path.display(), e[0])
        });
        let stem = path.file_stem().unwrap().to_str().unwrap();
        for backend in &backends {
            for with_tests in [true, false] {
                let direct = files_map(
                    backend
                        .emit_program(&program.modules, with_tests)
                        .unwrap_or_else(|e| panic!("{} emit: {e}", backend.name())),
                );
                let json = sudoc_ir::wire::to_wire_json(&program.modules)
                    .unwrap_or_else(|e| panic!("serialize {stem}: {e}"));
                let decoded = sudoc_ir::wire::from_wire_json(&json)
                    .unwrap_or_else(|e| panic!("deserialize {stem}: {e}"));
                let wire = files_map(
                    backend
                        .emit_program(&decoded, with_tests)
                        .unwrap_or_else(|e| panic!("{} emit: {e}", backend.name())),
                );
                assert_eq!(
                    direct, wire,
                    "wire-trip mismatch: module={stem} backend={} with_tests={with_tests}",
                    backend.name()
                );
            }
        }
    }
}
