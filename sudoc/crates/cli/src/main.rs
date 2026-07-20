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
//! Targets come from the harness registry (`all_backends`) plus any
//! `backends/*/*.sudoc-backend.json` manifests discovered under the current
//! working directory. Adding an in-tree backend crate to the registry, or
//! dropping a well-formed external manifest under `backends/<name>/`, makes
//! it appear for `--target` automatically. `--external` is an escape hatch
//! for manifests outside that layout (see spec/protocol.md).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sudoc_harness::{all_backends, discovered_backends, Backend};

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

macro_rules! outp {
    ($($arg:tt)*) => { write_stdout(format_args!($($arg)*)) };
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("build") => build(&args[1..]),
        Some("test") => test(&args[1..]),
        Some("conformance") => conformance(&args[1..]),
        _ => {
            let registry = match effective_registry(&[]) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            };
            let names: Vec<&str> = registry.iter().map(|b| b.name()).collect();
            eprintln!("usage: sudoc check FILE...");
            eprintln!(
                "       sudoc build --target T [--external MANIFEST ...] [--tests] [-o DIR] FILE..."
            );
            eprintln!("       sudoc test [--target T ...] [--external MANIFEST ...] FILE...");
            eprintln!(
                "       sudoc conformance [--target T ...] [--external MANIFEST ...] [DIR]"
            );
            eprintln!(
                "targets: {} (also auto-registers backends/*/*.sudoc-backend.json under cwd; --external is an escape hatch)",
                names.join(", ")
            );
            ExitCode::from(2)
        }
    }
}

fn load(path: &Path) -> Result<sudoc_types::Program, String> {
    sudoc_types::check_program(path).map_err(|es| format!("{}:{}", path.display(), es[0]))
}

/// Resolve a path for dedup: canonicalize when possible, otherwise use the
/// absolute path relative to cwd (so relative/absolute variants of the same
/// existing file still match when canonicalize works).
fn resolve_path(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Build the effective backend registry:
/// six in-tree + discovered under cwd + explicit `--external` (path-deduped).
/// Name collisions among any of those sources are a fatal error.
fn effective_registry(externals: &[PathBuf]) -> Result<Vec<Box<dyn Backend>>, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current_dir: {e}"))?;
    let discovered = discovered_backends(&cwd)?;

    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    // Track discovered manifest paths so a later --external of the same file is skipped.
    // ExternalBackend does not expose its path; re-scan discovered paths from disk layout
    // by collecting them during the explicit load loop only. For discovery, we record
    // nothing until we load --external — instead we load --external and skip if the
    // path was already used. Discovered backends don't store paths, so for dedup of
    // --external against discovery we canonicalize each --external path and also
    // re-list discovered manifest paths.
    let mut discovered_paths: HashSet<PathBuf> = HashSet::new();
    let backends_dir = cwd.join("backends");
    if let Ok(entries) = std::fs::read_dir(&backends_dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&sub) {
                for f in files.flatten() {
                    let p = f.path();
                    let is_manifest = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".sudoc-backend.json"));
                    if is_manifest && p.is_file() {
                        discovered_paths.insert(resolve_path(&p));
                    }
                }
            }
        }
    }

    let mut registry: Vec<Box<dyn Backend>> = all_backends();
    registry.extend(discovered);

    for m in externals {
        let resolved = resolve_path(m);
        if discovered_paths.contains(&resolved) || !seen_paths.insert(resolved) {
            // Already in registry via discovery, or duplicate --external.
            continue;
        }
        let b = sudoc_backend_ext::ExternalBackend::load(m)?;
        registry.push(Box::new(b));
    }

    // Name collisions are fatal.
    let mut first_idx: HashMap<String, usize> = HashMap::new();
    for (i, b) in registry.iter().enumerate() {
        let name = b.name().to_string();
        if let Some(_prev) = first_idx.insert(name.clone(), i) {
            return Err(format!(
                "backend name collision: '{name}' is registered more than once \
                 (in-tree, discovered under backends/, or via --external)"
            ));
        }
    }

    Ok(registry)
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

fn check(files: &[String]) -> ExitCode {
    let mut failed = false;
    for f in files {
        match load(Path::new(f)) {
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

fn build(args: &[String]) -> ExitCode {
    let mut target_names: Vec<String> = Vec::new();
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
                    Some(t) => target_names.push(t.clone()),
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

    let mut registry = match effective_registry(&externals) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    // Explicit --external backends that are not also named via --target still run
    // (legacy: build with only --external and no --target). Collect them by
    // tracking which names came solely from the external paths after discovery
    // dedup — simpler approach: if --target names given, resolve those; also
    // keep any backend that was loaded only via --external (not in discovery /
    // not in-tree). Spec says: build requires explicit target selection
    // (either --target naming something in the effective registry, or --external).
    let mut backends: Vec<Box<dyn Backend>> = Vec::new();
    if !target_names.is_empty() {
        for t in &target_names {
            match take_by_name(&mut registry, t) {
                Some(b) => backends.push(b),
                None => {
                    // Either unknown, or already taken (duplicate --target). Rebuild names from remaining + taken.
                    let mut names = available_names(&registry);
                    let taken: Vec<&str> = backends.iter().map(|b| b.name()).collect();
                    if !taken.is_empty() {
                        if !names.is_empty() {
                            names = format!("{}, {}", taken.join(", "), names);
                        } else {
                            names = taken.join(", ");
                        }
                    }
                    // Prefer full registry names for the error: re-build for message.
                    let full = match effective_registry(&externals) {
                        Ok(r) => available_names(&r),
                        Err(_) => names,
                    };
                    eprintln!("unknown target '{t}' (available: {full})");
                    return ExitCode::from(2);
                }
            }
        }
        // Also include --external-only backends that weren't selected by --target?
        // Spec: "build still requires explicit target selection (either --target
        // naming something in the effective registry, or --external)". If both
        // are given, current (pre-change) behavior loaded both --target and
        // --external into backends. Preserve that: after resolving --targets,
        // also push any remaining registry entries that came only from --external
        // (not in-tree, not discovered). That's hard without tagging.
        //
        // Pre-change: for each --target, push backend_by_name; then for each
        // --external, push ExternalBackend::load. So both lists contributed.
        // With discovery, --external that was also discovered is deduped.
        // If user passes --target py --external path/to/hs.json, both should run.
        // If user passes --target hs (discovered) and also --external hs.json, only once.
        //
        // After taking named targets, load any remaining unique --external that
        // wasn't already consumed. Easiest: for each external path, if its
        // backend name is still in registry, take it.
        for m in &externals {
            if let Ok(b) = sudoc_backend_ext::ExternalBackend::load(m) {
                let name = b.name().to_string();
                if let Some(taken) = take_by_name(&mut registry, &name) {
                    backends.push(taken);
                }
                // else already taken via --target (or duplicate external)
            }
        }
    } else {
        // No --target: only --external entries (if any) form the target list.
        // Discovery alone is not enough for build — user must opt in.
        for m in &externals {
            // Prefer the registry copy (already loaded, collision-checked) if present.
            if let Ok(b) = sudoc_backend_ext::ExternalBackend::load(m) {
                let name = b.name().to_string();
                if let Some(taken) = take_by_name(&mut registry, &name) {
                    backends.push(taken);
                } else {
                    // Was skipped as duplicate of discovery while no --target selected
                    // it — still count as explicit selection via --external.
                    backends.push(Box::new(b));
                }
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
        outln!("wrote {}", path.display());
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
    let mut target_names: Vec<String> = Vec::new();
    let mut had_target = false;
    let mut externals: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                had_target = true;
                match args.get(i) {
                    Some(t) => target_names.push(t.clone()),
                    None => {
                        eprintln!("unknown or missing target");
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
            d => dirs.push(PathBuf::from(d)),
        }
        i += 1;
    }

    let mut registry = match effective_registry(&externals) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let targets = if had_target {
        let mut selected = Vec::new();
        for t in &target_names {
            match take_by_name(&mut registry, t) {
                Some(b) => selected.push(b),
                None => {
                    // Rebuild full registry for the available-names list.
                    let full = match effective_registry(&externals) {
                        Ok(r) => available_names(&r),
                        Err(_) => available_names(&registry),
                    };
                    eprintln!("unknown target '{t}' (available: {full})");
                    return ExitCode::from(2);
                }
            }
        }
        // Pre-change also appended --external when --target was given.
        for m in &externals {
            if let Ok(b) = sudoc_backend_ext::ExternalBackend::load(m) {
                let name = b.name().to_string();
                if let Some(taken) = take_by_name(&mut registry, &name) {
                    selected.push(taken);
                }
            }
        }
        selected
    } else {
        // Entire effective registry (in-tree + discovered + unique --external).
        registry
    };

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
    outln!(
        "conformance: {} module(s) across targets: {}",
        files.len(),
        names.join(", ")
    );
    let mut failures = 0;
    for f in &files {
        match sudoc_harness::lockstep(f, &targets) {
            Ok(report) => {
                if report.all_pass() {
                    outln!("   ok        {}", report.module);
                } else {
                    failures += 1;
                    let (text, _) = sudoc_harness::render(&report);
                    outp!("{text}");
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!("   ERROR     {}: {e}", f.display());
            }
        }
    }
    outln!(
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
    let mut target_names: Vec<String> = Vec::new();
    let mut had_target = false;
    let mut externals: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                had_target = true;
                match args.get(i) {
                    Some(t) => target_names.push(t.clone()),
                    None => {
                        eprintln!("unknown or missing target");
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
            f => files.push(PathBuf::from(f)),
        }
        i += 1;
    }

    let mut registry = match effective_registry(&externals) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let targets = if had_target {
        let mut selected = Vec::new();
        for t in &target_names {
            match take_by_name(&mut registry, t) {
                Some(b) => selected.push(b),
                None => {
                    let full = match effective_registry(&externals) {
                        Ok(r) => available_names(&r),
                        Err(_) => available_names(&registry),
                    };
                    eprintln!("unknown or missing target (available: {full})");
                    return ExitCode::from(2);
                }
            }
        }
        for m in &externals {
            if let Ok(b) = sudoc_backend_ext::ExternalBackend::load(m) {
                let name = b.name().to_string();
                if let Some(taken) = take_by_name(&mut registry, &name) {
                    selected.push(taken);
                }
            }
        }
        selected
    } else {
        registry
    };

    if files.is_empty() {
        eprintln!("test needs at least one .sudo file");
        return ExitCode::from(2);
    }
    let mut green = true;
    for f in &files {
        match lockstep(f, &targets) {
            Ok(report) => {
                let (text, ok) = render(&report);
                outp!("{text}");
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
