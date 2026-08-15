//! The narrowed "harness": read the per-module tests manifest + N captured-run
//! files and diff the per-test outcomes across backends, reproducing the
//! monolithic harness's `render()` divergence table (Map/Set-order hints,
//! StackOverflow / sanitizer-crash "no result" annotations). Exits nonzero iff
//! not all-pass. This is the diff tool the decomposed Bazel lockstep DAG runs
//! (design §3); it *shares* the comparison code with the `harness` crate rather
//! than replacing it (the monolithic path stays until Phase 4).
//!
//! Usage: `lockstep_diff --module NAME --tests tests.json --run BACKEND=file.json [--run ...]`

use std::path::PathBuf;
use std::process::ExitCode;

use sudoc_harness::{diff, render, CapturedRun};

fn usage(msg: &str) -> ExitCode {
    eprintln!("lockstep_diff: {msg}");
    eprintln!(
        "usage: lockstep_diff --module NAME --tests tests.json --run BACKEND=file.json [--run ...]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The lockstep wire-protocol version, for `sudo_lockstep_test`'s run-time
    // matched-pair handshake: the launcher compares this against
    // `sudoc protocol-version` and fails loudly on a mismatch, so a release
    // sudoc paired with a mismatched lockstep_diff can't silently misdiff.
    if args.first().map(String::as_str) == Some("--protocol-version") {
        println!("{}", sudoc_harness::PROTOCOL_VERSION);
        return ExitCode::SUCCESS;
    }

    let mut module: Option<String> = None;
    let mut tests_path: Option<PathBuf> = None;
    let mut runs: Vec<(String, PathBuf)> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--module" => {
                i += 1;
                match args.get(i) {
                    Some(m) => module = Some(m.clone()),
                    None => return usage("--module needs a value"),
                }
            }
            "--tests" => {
                i += 1;
                match args.get(i) {
                    Some(p) => tests_path = Some(PathBuf::from(p)),
                    None => return usage("--tests needs a value"),
                }
            }
            "--run" => {
                i += 1;
                match args.get(i) {
                    Some(spec) => match spec.split_once('=') {
                        Some((name, path)) => runs.push((name.to_string(), PathBuf::from(path))),
                        None => return usage("--run needs BACKEND=FILE"),
                    },
                    None => return usage("--run needs a value"),
                }
            }
            other => return usage(&format!("unexpected arg {other:?}")),
        }
        i += 1;
    }

    let Some(module) = module else {
        return usage("--module is required");
    };
    let Some(tests_path) = tests_path else {
        return usage("--tests is required");
    };
    if runs.is_empty() {
        return usage("at least one --run BACKEND=FILE is required");
    }

    let tests_json = match std::fs::read_to_string(&tests_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lockstep_diff: {}: {e}", tests_path.display());
            return ExitCode::from(2);
        }
    };
    let tests_manifest: Vec<String> = match serde_json::from_str(&tests_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "lockstep_diff: parsing tests manifest {}: {e}",
                tests_path.display()
            );
            return ExitCode::from(2);
        }
    };
    // A module with zero tests would make every backend trivially "agree" and
    // pass vacuously — almost always a mistake (an empty module, or emit-tests
    // silently producing nothing). Refuse it rather than report a hollow green.
    if tests_manifest.is_empty() {
        eprintln!(
            "lockstep_diff: module '{module}' has no tests — refusing a vacuous pass \
             (a lockstep module must declare at least one `test`)"
        );
        return ExitCode::FAILURE;
    }

    let mut captured: Vec<(String, CapturedRun)> = Vec::with_capacity(runs.len());
    for (name, path) in &runs {
        let s = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("lockstep_diff: {}: {e}", path.display());
                return ExitCode::from(2);
            }
        };
        let run: CapturedRun = match serde_json::from_str(&s) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "lockstep_diff: parsing captured run {}: {e}",
                    path.display()
                );
                return ExitCode::from(2);
            }
        };
        captured.push((name.clone(), run));
    }

    let report = diff(&module, &tests_manifest, &captured);
    let (text, all_green) = render(&report);
    print!("{text}");
    if all_green {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
