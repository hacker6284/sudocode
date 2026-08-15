//! Never-fail run wrapper for the decomposed lockstep DAG (design §3). Two modes,
//! both ALWAYS exit 0 for the wrapped work so a crashing/failing backend still
//! yields a captured-run file and its "no result" divergence signal survives:
//!
//!   capture_run --recipe RECIPE.json --dir DIR --out OUT.json
//!       Run a backend's TestRecipe (`sudoc emit-recipe`): each build step in
//!       DIR, then the run command in DIR, capturing {stdout, stderr, exit_code}.
//!       A failed build step becomes the captured outcome (nonzero exit) — the
//!       backend's tests then read as Missing/divergence, never a silent pass.
//!
//!   capture_run --out OUT.json -- CMD [ARGS...]
//!       Run one command directly (no recipe).
//!
//! Only wrapper-usage errors (bad args, unreadable recipe, unwritable output)
//! exit nonzero — those are real rule misconfigurations.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sudoc_harness::CapturedRun;
use sudoc_sdk::TestRecipe;

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Run one argv in `dir`, returning None on success or Some(captured) on the
/// first failure (spawn error or nonzero exit) so the caller can short-circuit.
fn run_build_step(step: &[String], dir: &Path) -> Option<CapturedRun> {
    if step.is_empty() {
        return None;
    }
    match Command::new(&step[0])
        .args(&step[1..])
        .current_dir(dir)
        .output()
    {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(CapturedRun {
            stdout: String::new(),
            stderr: format!("build step {step:?} failed:\n{}", lossy(o.stderr)),
            exit_code: o.status.code().unwrap_or(-1),
        }),
        Err(e) => Some(CapturedRun {
            stdout: String::new(),
            stderr: format!("build step {:?}: {e}", step[0]),
            exit_code: -1,
        }),
    }
}

fn run_recipe(recipe: &TestRecipe, dir: &Path) -> CapturedRun {
    for step in &recipe.build {
        if let Some(failed) = run_build_step(step, dir) {
            return failed;
        }
    }
    if recipe.run.is_empty() {
        return CapturedRun {
            stdout: String::new(),
            stderr: "empty run command".into(),
            exit_code: -1,
        };
    }
    match Command::new(&recipe.run[0])
        .args(&recipe.run[1..])
        .current_dir(dir)
        .output()
    {
        Ok(o) => CapturedRun {
            stdout: lossy(o.stdout),
            stderr: lossy(o.stderr),
            exit_code: o.status.code().unwrap_or(-1),
        },
        Err(e) => CapturedRun {
            stdout: String::new(),
            stderr: format!("run {:?}: {e}", recipe.run[0]),
            exit_code: -1,
        },
    }
}

fn run_direct(cmd: &[String]) -> CapturedRun {
    match Command::new(&cmd[0]).args(&cmd[1..]).output() {
        Ok(o) => CapturedRun {
            stdout: lossy(o.stdout),
            stderr: lossy(o.stderr),
            exit_code: o.status.code().unwrap_or(-1),
        },
        Err(e) => CapturedRun {
            stdout: String::new(),
            stderr: format!("capture_run: failed to spawn {:?}: {e}", cmd[0]),
            exit_code: -1,
        },
    }
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("capture_run: {msg}");
    eprintln!("usage: capture_run --recipe R.json --dir DIR --out OUT.json");
    eprintln!("   or: capture_run --out OUT.json -- CMD [ARGS...]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<PathBuf> = None;
    let mut recipe_path: Option<PathBuf> = None;
    let mut dir: Option<PathBuf> = None;
    let mut cmd: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                match args.get(i) {
                    Some(p) => out = Some(PathBuf::from(p)),
                    None => return usage("--out needs a value"),
                }
            }
            "--recipe" => {
                i += 1;
                match args.get(i) {
                    Some(p) => recipe_path = Some(PathBuf::from(p)),
                    None => return usage("--recipe needs a value"),
                }
            }
            "--dir" => {
                i += 1;
                match args.get(i) {
                    Some(p) => dir = Some(PathBuf::from(p)),
                    None => return usage("--dir needs a value"),
                }
            }
            "--" => {
                cmd = args[i + 1..].to_vec();
                break;
            }
            other => return usage(&format!("unexpected arg {other:?}")),
        }
        i += 1;
    }

    let Some(out) = out else {
        return usage("--out is required");
    };

    let captured = if let Some(recipe_path) = recipe_path {
        let dir = dir.unwrap_or_else(|| PathBuf::from("."));
        let text = match std::fs::read_to_string(&recipe_path) {
            Ok(t) => t,
            Err(e) => return usage(&format!("{}: {e}", recipe_path.display())),
        };
        let recipe: TestRecipe = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => return usage(&format!("parsing recipe {}: {e}", recipe_path.display())),
        };
        run_recipe(&recipe, &dir)
    } else if !cmd.is_empty() {
        run_direct(&cmd)
    } else {
        return usage("need --recipe or -- CMD");
    };

    let json = match serde_json::to_string_pretty(&captured) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("capture_run: serialize: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::write(&out, json) {
        eprintln!("capture_run: {}: {e}", out.display());
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
