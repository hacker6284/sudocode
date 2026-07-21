use std::path::PathBuf;
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-cli-equiv-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn std_import_generates_identical_code_to_file_import() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let stdlib_regex = repo_root.join("stdlib/regex.sudo");
    let stdlib_strings = repo_root.join("stdlib/strings.sudo");

    let body = "func f() -> bool\n    r = regex.regex_search(\"a+\", \"aaa\", false)\n    ok = strings.to_upper(\"x\") == \"X\"\n    match r\n        case Ok(v)\n            return v and ok\n        case Err(e)\n            return false\n";

    // A: std imports.
    let dir_a = temp_dir("std");
    std::fs::write(
        dir_a.join("mod.sudo"),
        format!("import std.regex\nimport std.strings\n\n{body}"),
    )
    .unwrap();

    // B: same body, plain imports, with regex.sudo/strings.sudo copied
    // beside it (rule 1: the importing file's own directory).
    let dir_b = temp_dir("file");
    std::fs::copy(&stdlib_regex, dir_b.join("regex.sudo")).unwrap();
    std::fs::copy(&stdlib_strings, dir_b.join("strings.sudo")).unwrap();
    std::fs::write(dir_b.join("mod.sudo"), format!("import regex\nimport strings\n\n{body}")).unwrap();

    let out_a = temp_dir("out-std");
    let out_b = temp_dir("out-file");

    for (dir, out) in [(&dir_a, &out_a), (&dir_b, &out_b)] {
        let output = Command::new(env!("CARGO_BIN_EXE_sudoc"))
            .args([
                "build",
                "--target",
                "py",
                "-o",
                out.to_str().unwrap(),
                dir.join("mod.sudo").to_str().unwrap(),
            ])
            .output()
            .expect("failed to run sudoc build");
        assert!(
            output.status.success(),
            "build failed for {}: stdout={} stderr={}",
            dir.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Per-module generated files must be byte-identical. Implementation files
    // are always `_{module}_impl.py`; host-facing `{module}.py` API files are
    // only emitted for modules with at least one `export func` (regex here).
    // Shared runtime `_sudo_rt.py` is trivially identical in both outputs.
    for fname in ["_mod_impl.py", "_regex_impl.py", "_strings_impl.py", "regex.py"] {
        let a = std::fs::read(out_a.join(fname))
            .unwrap_or_else(|e| panic!("missing {fname} in std output: {e}"));
        let b = std::fs::read(out_b.join(fname))
            .unwrap_or_else(|e| panic!("missing {fname} in file output: {e}"));
        assert_eq!(
            a, b,
            "{fname} differs between std import and file import output"
        );
    }

    std::fs::remove_dir_all(&dir_a).ok();
    std::fs::remove_dir_all(&dir_b).ok();
    std::fs::remove_dir_all(&out_a).ok();
    std::fs::remove_dir_all(&out_b).ok();
}
