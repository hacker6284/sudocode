//! sudoc — the sudo transpiler CLI.
//!
//! Usage:
//! ```text
//! sudoc check FILE...
//! sudoc build --target T [--external MANIFEST ...] [--tests] [-o DIR] FILE...
//! sudoc test [--target T ...] [--external MANIFEST ...] FILE...
//!     lockstep: run tests in every target and diff the outcomes; divergence
//!     is a first-class failure
//! sudoc conformance [--target T ...] [--external MANIFEST ...] [DIR]
//!     the spec's executable form
//! ```
//!
//! Targets come from the harness registry (`all_backends`) — adding a backend
//! crate to the registry makes it appear everywhere here automatically.
//! External backends are loaded from a manifest via `--external` (see
//! spec/protocol.md).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sudoc_harness::{all_backends, backend_by_name, Backend};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("build") => build(&args[1..]),
        Some("test") => test(&args[1..]),
        Some("conformance") => conformance(&args[1..]),
        _ => {
            let backends = all_backends();
            let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
            eprintln!("usage: sudoc check FILE...");
            eprintln!(
                "       sudoc build --target T [--external MANIFEST ...] [--tests] [-o DIR] FILE..."
            );
            eprintln!("       sudoc test [--target T ...] [--external MANIFEST ...] FILE...");
            eprintln!(
                "       sudoc conformance [--target T ...] [--external MANIFEST ...] [DIR]"
            );
            eprintln!("targets: {}", names.join(", "));
            ExitCode::from(2)
        }
    }
}

fn load(path: &Path) -> Result<sudoc_types::Program, String> {
    sudoc_types::check_program(path).map_err(|es| format!("{}:{}", path.display(), es[0]))
}

fn check(files: &[String]) -> ExitCode {
    let mut failed = false;
    for f in files {
        match load(Path::new(f)) {
            Ok(_) => println!("{f}: ok"),
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

fn build(args: &[String]) -> ExitCode {
    let mut targets: Vec<String> = Vec::new();
    let mut externals: Vec<PathBuf> = Vec::new();
    let mut out_dir = PathBuf::from(".");
    let mut with_tests = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                match args.get(i) {
                    Some(t) => targets.push(t.clone()),
                    None => {
                        eprintln!("--target needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            "--external" => {
                i += 1;
                match args.get(i) {
                    Some(m) => externals.push(PathBuf::from(m)),
                    None => {
                        eprintln!("--external needs a value");
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
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }
    let mut backends: Vec<Box<dyn Backend>> = Vec::new();
    for t in &targets {
        match backend_by_name(t) {
            Some(b) => backends.push(b),
            None => {
                let backends = all_backends();
                let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
                eprintln!("unknown target '{t}' (available: {})", names.join(", "));
                return ExitCode::from(2);
            }
        }
    }
    for m in &externals {
        match sudoc_backend_ext::ExternalBackend::load(m) {
            Ok(b) => backends.push(Box::new(b)),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
    }
    if backends.is_empty() || files.is_empty() {
        eprintln!("build needs --target or --external, and at least one file");
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
        println!("wrote {}", path.display());
        true
    };
    for f in &files {
        let program = match load(f) {
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

/// Run the conformance corpus (plus any extra dirs) across targets.
fn conformance(args: &[String]) -> ExitCode {
    let mut targets: Vec<Box<dyn Backend>> = Vec::new();
    let mut had_target = false;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                had_target = true;
                match args.get(i).and_then(|t| backend_by_name(t)) {
                    Some(b) => targets.push(b),
                    None => {
                        eprintln!("unknown target");
                        return ExitCode::from(2);
                    }
                }
            }
            "--external" => {
                i += 1;
                match args.get(i) {
                    Some(m) => match sudoc_backend_ext::ExternalBackend::load(Path::new(m)) {
                        Ok(b) => targets.push(Box::new(b)),
                        Err(e) => {
                            eprintln!("{e}");
                            return ExitCode::from(2);
                        }
                    },
                    None => {
                        eprintln!("--external needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            d => dirs.push(PathBuf::from(d)),
        }
        i += 1;
    }
    // Externals already pushed during parse. If no --target was given, prepend
    // the six defaults (externals stay appended after them).
    if !had_target {
        let mut defaults = all_backends();
        defaults.append(&mut targets);
        targets = defaults;
    }
    if dirs.is_empty() {
        dirs.push(PathBuf::from("conformance/semantics"));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for d in &dirs {
        let Ok(entries) = std::fs::read_dir(d) else {
            eprintln!("cannot read {}", d.display());
            return ExitCode::FAILURE;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "sudo") {
                files.push(p);
            }
        }
    }
    files.sort();
    let names: Vec<&str> = targets.iter().map(|b| b.name()).collect();
    println!(
        "conformance: {} module(s) across targets: {}",
        files.len(),
        names.join(", ")
    );
    let mut failures = 0;
    for f in &files {
        match sudoc_harness::lockstep(f, &targets) {
            Ok(report) => {
                if report.all_pass() {
                    println!("   ok        {}", report.module);
                } else {
                    failures += 1;
                    let (text, _) = sudoc_harness::render(&report);
                    print!("{text}");
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!("   ERROR     {}: {e}", f.display());
            }
        }
    }
    println!(
        "# {}/{} modules conform",
        files.len() - failures,
        files.len()
    );
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn test(args: &[String]) -> ExitCode {
    use sudoc_harness::{lockstep, render};
    let mut targets: Vec<Box<dyn Backend>> = Vec::new();
    let mut had_target = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                had_target = true;
                match args.get(i).and_then(|t| backend_by_name(t)) {
                    Some(b) => targets.push(b),
                    None => {
                        let backends = all_backends();
                        let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
                        eprintln!("unknown or missing target (available: {})", names.join(", "));
                        return ExitCode::from(2);
                    }
                }
            }
            "--external" => {
                i += 1;
                match args.get(i) {
                    Some(m) => match sudoc_backend_ext::ExternalBackend::load(Path::new(m)) {
                        Ok(b) => targets.push(Box::new(b)),
                        Err(e) => {
                            eprintln!("{e}");
                            return ExitCode::from(2);
                        }
                    },
                    None => {
                        eprintln!("--external needs a value");
                        return ExitCode::from(2);
                    }
                }
            }
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }
    if !had_target {
        let mut defaults = all_backends();
        defaults.append(&mut targets);
        targets = defaults;
    }
    if files.is_empty() {
        eprintln!("test needs at least one .sudo file");
        return ExitCode::from(2);
    }
    let mut green = true;
    for f in &files {
        match lockstep(f, &targets) {
            Ok(report) => {
                let (text, ok) = render(&report);
                print!("{text}");
                green &= ok;
            }
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                green = false;
            }
        }
    }
    if green {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
