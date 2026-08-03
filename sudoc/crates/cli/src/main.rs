//! sudoc — the sudo transpiler CLI.
//!
//! Usage:
//! ```text
//! sudoc check [-I DIR]... FILE...
//! sudoc build --target T [--tests] [-o DIR] [-I DIR]... FILE...
//! sudoc emit-ir / emit-tests / emit-recipe — the decomposed lockstep contracts
//!     the Bazel build consumes (codegen, tests manifest, per-backend recipe).
//! ```
//!
//! The monolithic `sudoc test` / `sudoc conformance` runners were retired in the
//! Bazel migration (Phase 4): lockstep now runs as the decomposed Bazel DAG
//! (codegen + recipe + `capture_run` + `lockstep_diff`). The harness still
//! exposes `lockstep()` for its own integration tests.
//!
//! Targets are the six in-tree backends (`all_backends`). Runtime manifest
//! discovery and the `--external` escape hatch were removed in Phase 5: external
//! backends (e.g. `hs`) are now driven entirely by the Bazel `sudo_external_backend`
//! rule over the `emit-ir` + emit-protocol boundary (spec/protocol.md), not by
//! `sudoc` at run time.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sudoc_harness::{all_backends, Backend};

/// Route every stdout write through this so a downstream reader closing
/// the pipe early (`head`, `grep`, ...) exits us cleanly at code 0
/// instead of panicking on a `BrokenPipe` write error.
fn write_stdout(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let stdout = std::io::stdout();
    if let Err(e) = stdout.lock().write_fmt(args) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("sudoc: stdout write error: {e}");
        std::process::exit(1);
    }
}

macro_rules! outln {
    () => { write_stdout(format_args!("\n")) };
    ($($arg:tt)*) => { write_stdout(format_args!("{}\n", format_args!($($arg)*))) };
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("build") => build(&args[1..]),
        Some("emit-ir") => emit_ir(&args[1..]),
        Some("emit-tests") => emit_tests(&args[1..]),
        Some("emit-recipe") => emit_recipe(&args[1..]),
        _ => {
            let registry = all_backends();
            let names: Vec<&str> = registry.iter().map(|b| b.name()).collect();
            eprintln!("usage: sudoc check [-I DIR]... FILE...");
            eprintln!("       sudoc build --target T [--tests] [-o DIR] [-I DIR]... FILE...");
            eprintln!("       sudoc emit-ir [-I DIR]... [-o FILE] FILE");
            eprintln!("       sudoc emit-tests [-I DIR]... [-o FILE] FILE");
            eprintln!("       sudoc emit-recipe --target T [-o FILE] FILE");
            eprintln!("targets: {}", names.join(", "));
            ExitCode::from(2)
        }
    }
}

fn load(path: &Path, search_paths: &[PathBuf]) -> Result<sudoc_types::Program, String> {
    // Print every error the checker returns, not just the first — the checker
    // is single-error today (see check_program_with), but this no longer drops
    // es[1..] silently if it starts accumulating. (F10)
    sudoc_types::check_program_with(path, search_paths).map_err(|es| {
        es.iter()
            .map(|e| format!("{}:{}", path.display(), e))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Look up `name` in the registry; on success, remove and return that entry
/// so each backend is used at most once when resolving multiple --target flags.
fn take_by_name(registry: &mut Vec<Box<dyn Backend>>, name: &str) -> Option<Box<dyn Backend>> {
    let idx = registry.iter().position(|b| b.name() == name)?;
    Some(registry.swap_remove(idx))
}

fn available_names(registry: &[Box<dyn Backend>]) -> String {
    registry
        .iter()
        .map(|b| b.name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn check(args: &[String]) -> ExitCode {
    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-I" => {
                i += 1;
                match args.get(i) {
                    Some(d) => search_paths.push(PathBuf::from(d)),
                    None => {
                        eprintln!("-I needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    let mut failed = false;
    for f in &files {
        match load(Path::new(f), &search_paths) {
            Ok(_) => outln!("{f}: ok"),
            Err(e) => {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Parsed args shared by `emit-ir` / `emit-tests`: exactly one entry file,
/// optional `-I DIR` search paths, optional `-o FILE` output.
struct EmitArgs {
    search_paths: Vec<PathBuf>,
    out: Option<PathBuf>,
    file: PathBuf,
}

fn parse_emit_args(cmd: &str, args: &[String]) -> Result<EmitArgs, ExitCode> {
    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-I" => {
                i += 1;
                match args.get(i) {
                    Some(d) => search_paths.push(PathBuf::from(d)),
                    None => {
                        eprintln!("-I needs a value");
                        return Err(ExitCode::from(2));
                    }
                }
            }
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(d) => out = Some(PathBuf::from(d)),
                    None => {
                        eprintln!("-o needs a value");
                        return Err(ExitCode::from(2));
                    }
                }
            }
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }
    if files.len() != 1 {
        eprintln!("{cmd} needs exactly one entry file");
        return Err(ExitCode::from(2));
    }
    Ok(EmitArgs { search_paths, out, file: files.into_iter().next().unwrap() })
}

/// Write `content` to `out` (or stdout when `None`).
fn emit_write(out: Option<&Path>, content: &str) -> ExitCode {
    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(path, content) {
                eprintln!("{}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
        None => outln!("{content}"),
    }
    ExitCode::SUCCESS
}

/// `sudoc emit-ir [-I DIR]... [-o FILE] FILE` — the checked program's IR
/// modules as JSON: the documented sudoc↔emitter boundary artifact
/// (spec/protocol.md), consumed by the external Haskell emitter (Phase 3).
/// Dependencies first, entry module last (check_program order).
fn emit_ir(args: &[String]) -> ExitCode {
    let parsed = match parse_emit_args("emit-ir", args) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let program = match load(&parsed.file, &parsed.search_paths) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let json = match serde_json::to_string_pretty(&program.modules) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("emit-ir: serializing IR: {e}");
            return ExitCode::FAILURE;
        }
    };
    emit_write(parsed.out.as_deref(), &json)
}

/// `sudoc emit-tests [-I DIR]... [-o FILE] FILE` — the entry module's test
/// function names as a JSON array (the per-module tests manifest the lockstep
/// DAG diffs against, so a test absent from every backend can't vanish).
fn emit_tests(args: &[String]) -> ExitCode {
    let parsed = match parse_emit_args("emit-tests", args) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let program = match load(&parsed.file, &parsed.search_paths) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let entry = match program.modules.last() {
        Some(m) => m,
        None => {
            eprintln!("emit-tests: {}: no modules", parsed.file.display());
            return ExitCode::FAILURE;
        }
    };
    let names = sudoc_ir::names::test_fn_names(&entry.tests);
    let json = match serde_json::to_string_pretty(&names) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("emit-tests: serializing names: {e}");
            return ExitCode::FAILURE;
        }
    };
    emit_write(parsed.out.as_deref(), &json)
}

/// `sudoc emit-recipe --target T [-o FILE] FILE` — the backend's build+run
/// TestRecipe for the entry module as JSON. The single source of truth the
/// decomposed lockstep run-leaf executes (build steps + the run command whose
/// stdout is the outcome protocol), so Bazel never re-encodes per-backend
/// compile flags / sanitizers / libm.
fn emit_recipe(args: &[String]) -> ExitCode {
    let mut target: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                match args.get(i) {
                    Some(t) => target = Some(t.clone()),
                    None => {
                        eprintln!("--target needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(d) => out = Some(PathBuf::from(d)),
                    None => {
                        eprintln!("-o needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }
    let Some(target) = target else {
        eprintln!("emit-recipe needs --target T");
        return ExitCode::from(2);
    };
    if files.len() != 1 {
        eprintln!("emit-recipe needs exactly one entry file");
        return ExitCode::from(2);
    }
    let stem = match files[0].file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => {
            eprintln!("emit-recipe: bad entry file name");
            return ExitCode::from(2);
        }
    };
    let registry = all_backends();
    let backend = match registry.iter().find(|b| b.name() == target) {
        Some(b) => b,
        None => {
            eprintln!("emit-recipe: unknown target '{target}'");
            return ExitCode::from(2);
        }
    };
    let recipe = backend.test_recipe(&stem);
    let json = match serde_json::to_string_pretty(&recipe) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("emit-recipe: serialize: {e}");
            return ExitCode::from(2);
        }
    };
    emit_write(out.as_deref(), &json)
}

fn build(args: &[String]) -> ExitCode {
    let mut target_names: Vec<String> = Vec::new();
    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut out_dir = PathBuf::from(".");
    let mut with_tests = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                match args.get(i) {
                    Some(t) => target_names.push(t.clone()),
                    None => {
                        eprintln!("--target needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            "--tests" => with_tests = true,
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(d) => out_dir = PathBuf::from(d),
                    None => {
                        eprintln!("-o needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            "-I" => {
                i += 1;
                match args.get(i) {
                    Some(d) => search_paths.push(PathBuf::from(d)),
                    None => {
                        eprintln!("-I needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }

    let mut registry = all_backends();

    // Resolve each --target against the six in-tree backends. (External backends
    // are no longer a `sudoc build` target — they run via the Bazel
    // `sudo_external_backend` rule over the emit-ir/emit-protocol boundary.)
    let mut backends: Vec<Box<dyn Backend>> = Vec::new();
    for t in &target_names {
        match take_by_name(&mut registry, t) {
            Some(b) => backends.push(b),
            None => {
                let taken: Vec<&str> = backends.iter().map(|b| b.name()).collect();
                let full = available_names(&all_backends());
                eprintln!(
                    "unknown target '{t}' (available: {full}{})",
                    if taken.is_empty() {
                        String::new()
                    } else {
                        format!("; already selected: {}", taken.join(", "))
                    }
                );
                return ExitCode::from(2);
            }
        }
    }

    if backends.is_empty() || files.is_empty() {
        eprintln!("build needs --target, and at least one file");
        return ExitCode::from(2);
    }
    if std::fs::create_dir_all(&out_dir).is_err() {
        eprintln!("cannot create output directory {}", out_dir.display());
        return ExitCode::FAILURE;
    }
    let write = |path: &Path, content: &str| -> bool {
        if let Err(e) = std::fs::write(path, content) {
            eprintln!("{}: {e}", path.display());
            return false;
        }
        outln!("wrote {}", path.display());
        true
    };
    for f in &files {
        let program = match load(f, &search_paths) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        for b in &backends {
            let files = match b.emit_program(&program.modules, with_tests) {
                Ok(files) => files,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            for gf in files.into_iter().chain(b.runtime_files()) {
                if !write(&out_dir.join(&gf.path), &gf.contents) {
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    ExitCode::SUCCESS
}