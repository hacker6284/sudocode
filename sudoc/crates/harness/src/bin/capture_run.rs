//! Never-fail run wrapper: run a command, capture {stdout, stderr, exit_code}
//! as JSON to `--out`, and ALWAYS exit 0 for the *wrapped* command's outcome.
//! A crashing runner (StackOverflow, ASan abort) must still produce a
//! captured-run file so the decomposed lockstep DAG's "no result" divergence
//! signal survives (design §3). Only wrapper-usage errors (bad args, can't
//! write the output file) exit nonzero — those are real rule misconfigurations.
//!
//! Usage: `capture_run --out FILE -- CMD [ARGS...]`

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use sudoc_harness::CapturedRun;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<PathBuf> = None;
    let mut cmd: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                match args.get(i) {
                    Some(p) => out = Some(PathBuf::from(p)),
                    None => {
                        eprintln!("capture_run: --out needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            "--" => {
                cmd = args[i + 1..].to_vec();
                break;
            }
            other => {
                eprintln!("capture_run: unexpected arg {other:?} (usage: --out FILE -- CMD ...)");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let Some(out) = out else {
        eprintln!("capture_run: --out is required");
        return ExitCode::from(2);
    };
    if cmd.is_empty() {
        eprintln!("capture_run: no command after --");
        return ExitCode::from(2);
    }

    // A spawn failure (command not found) is itself the captured outcome —
    // record it rather than failing, keeping the wrapper never-fail. A signal
    // death has no exit code; record -1 so `diff` treats it as a nonzero crash.
    let captured = match Command::new(&cmd[0]).args(&cmd[1..]).output() {
        Ok(o) => CapturedRun {
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            exit_code: o.status.code().unwrap_or(-1),
        },
        Err(e) => CapturedRun {
            stdout: String::new(),
            stderr: format!("capture_run: failed to spawn {:?}: {e}", cmd[0]),
            exit_code: -1,
        },
    };

    let json = match serde_json::to_string_pretty(&captured) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("capture_run: serialize: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::write(&out, json) {
        // Can't write the captured file → the diff has no input: a real error.
        eprintln!("capture_run: {}: {e}", out.display());
        return ExitCode::from(2);
    }
    // Always exit 0 for the wrapped command's outcome.
    ExitCode::SUCCESS
}
