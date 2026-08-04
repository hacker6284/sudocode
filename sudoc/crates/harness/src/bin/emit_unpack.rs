//! Unpack an external backend's emit response into a source tree (Bazel
//! migration Phase 5). Reads one `{"files":[{"path","contents"},…]}` JSON
//! object (the emit protocol response, spec/protocol.md §2) on stdin and writes
//! each file under `-o DIR`.
//!
//!   emit_unpack -o DIR   < response.json
//!
//! This is the surviving-protocol-boundary counterpart to `capture_run`: the
//! `sudo_external_backend` codegen action pipes `sudoc emit-ir` (wrapped in the
//! emit request envelope) through the emitter and unpacks the response here —
//! replacing the `--external`/`discovered_backends()` registration path that
//! used to spawn the emitter and write its files from inside `sudoc build`.
//!
//! Path safety: each response path must be relative with no `..` components (no
//! absolute paths, no escaping the output dir). An `{"error": "..."}` response,
//! a missing/mistyped
//! `files`, or an unsafe path is a hard error (nonzero exit) — a real codegen
//! failure, not something to swallow.

use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

fn fail(msg: &str) -> ExitCode {
    eprintln!("emit_unpack: {msg}");
    ExitCode::from(1)
}

/// Reject absolute paths and any `..` component (spec/protocol.md §2).
fn validate_path(p: &str) -> Result<(), String> {
    let path = Path::new(p);
    for comp in path.components() {
        match comp {
            Component::ParentDir => return Err(format!("response path '{p}' contains '..'")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("response path '{p}' is absolute"))
            }
            _ => {}
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(d) => out_dir = Some(PathBuf::from(d)),
                    None => return fail("-o needs a value"),
                }
            }
            other => return fail(&format!("unexpected arg {other:?}; usage: emit_unpack -o DIR")),
        }
        i += 1;
    }
    let Some(out_dir) = out_dir else { return fail("-o DIR is required") };

    let mut input = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
        return fail(&format!("reading stdin: {e}"));
    }

    let value: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => return fail(&format!("malformed emit response: {e}")),
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return fail("malformed emit response: expected a JSON object"),
    };
    if let Some(err) = obj.get("error") {
        let msg = err.as_str().unwrap_or("non-string error field");
        return fail(&format!("emitter error: {msg}"));
    }
    let files = match obj.get("files").and_then(|v| v.as_array()) {
        Some(f) => f,
        None => return fail("malformed emit response: missing/non-array 'files' (and no 'error')"),
    };

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return fail(&format!("{}: {e}", out_dir.display()));
    }

    for item in files {
        let path = match item.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return fail("malformed emit response: file missing string 'path'"),
        };
        let contents = match item.get("contents").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return fail("malformed emit response: file missing string 'contents'"),
        };
        if let Err(e) = validate_path(path) {
            return fail(&e);
        }
        let dest = out_dir.join(path);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return fail(&format!("{}: {e}", parent.display()));
            }
        }
        if let Err(e) = std::fs::write(&dest, contents) {
            return fail(&format!("{}: {e}", dest.display()));
        }
    }
    ExitCode::SUCCESS
}
