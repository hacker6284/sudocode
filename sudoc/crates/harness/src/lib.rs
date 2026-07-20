//! The lockstep harness (lockstep.md §2–3): build a module's tests for every
//! configured target, execute them, and diff the per-test outcomes across
//! targets. Traps compare by kind only. `pass` in one target and `trap` in
//! another — or different trap kinds — is a **divergence**, reported as a
//! first-class failure distinct from "fails identically everywhere".

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Trap(String),
    /// The runner died without reporting this test (crash, missing output).
    Missing,
}

pub use sudoc_sdk::Backend;

/// Every backend compiled into this sudoc. New backends register here and
/// are immediately available to `sudoc build/test/conformance`.
pub fn all_backends() -> Vec<Box<dyn Backend>> {
    vec![
        Box::new(sudoc_backend_py::PythonBackend),
        Box::new(sudoc_backend_c::CBackend),
        Box::new(sudoc_backend_js::JsBackend),
    ]
}

pub fn backend_by_name(name: &str) -> Option<Box<dyn Backend>> {
    all_backends().into_iter().find(|b| b.name() == name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every target passed.
    Pass,
    /// Every target trapped with the same kind: the test fails, but the
    /// implementations agree — an algorithm bug, not a lockstep bug.
    ConsistentFailure(String),
    /// Targets disagree.
    Divergence,
}

#[derive(Debug, Clone)]
pub struct TestReport {
    pub name: String,
    /// (backend name, outcome) per target.
    pub outcomes: Vec<(String, Outcome)>,
    /// Per-target diagnostic detail (e.g. serialized assert operands).
    pub details: Vec<(String, String)>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone)]
pub struct ModuleReport {
    pub module: String,
    pub tests: Vec<TestReport>,
}

impl ModuleReport {
    pub fn all_pass(&self) -> bool {
        self.tests.iter().all(|t| t.verdict == Verdict::Pass)
    }
    pub fn divergences(&self) -> usize {
        self.tests.iter().filter(|t| t.verdict == Verdict::Divergence).count()
    }
}

#[derive(Debug)]
pub enum HarnessError {
    Check(String),
    Build { target: String, detail: String },
    Run { target: String, detail: String },
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Check(e) => write!(f, "check failed: {e}"),
            HarnessError::Build { target, detail } => {
                write!(f, "building for {target} failed: {detail}")
            }
            HarnessError::Run { target, detail } => {
                write!(f, "running under {target} failed: {detail}")
            }
        }
    }
}

/// One parsed TAP line: name, outcome, and optional diagnostic detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapLine {
    pub name: String,
    pub outcome: Outcome,
    pub detail: Option<String>,
}

/// Parse a runner's TAP-ish stdout.
/// Lines: `ok N - name` / `not ok N - name [Kind]` / `not ok N - name [Kind: detail]`.
pub fn parse_tap(stdout: &str) -> Vec<TapLine> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("not ok ") {
            let Some((_, rest)) = rest.split_once(" - ") else { continue };
            let (name, kind, detail) = match rest.split_once(" [") {
                Some((name, bracket)) => {
                    let inner = bracket.strip_suffix(']').unwrap_or(bracket);
                    match inner.split_once(": ") {
                        Some((kind, detail)) => {
                            (name.to_string(), kind.to_string(), Some(detail.to_string()))
                        }
                        None => (name.to_string(), inner.to_string(), None),
                    }
                }
                None => (rest.to_string(), "Unknown".to_string(), None),
            };
            out.push(TapLine { name, outcome: Outcome::Trap(kind), detail });
        } else if let Some(rest) = line.strip_prefix("ok ") {
            let Some((_, name)) = rest.split_once(" - ") else { continue };
            out.push(TapLine { name: name.to_string(), outcome: Outcome::Pass, detail: None });
        }
    }
    out
}

/// Run one module's tests under every target and produce the lockstep report.
pub fn lockstep(
    source_path: &Path,
    targets: &[Box<dyn Backend>],
) -> Result<ModuleReport, HarnessError> {
    let src = std::fs::read_to_string(source_path)
        .map_err(|e| HarnessError::Check(format!("{}: {e}", source_path.display())))?;
    let module_name: String = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| HarnessError::Check("bad file name".into()))?
        .to_string();
    let _ = src;
    let program = sudoc_types::check_program(source_path)
        .map_err(|es| HarnessError::Check(format!("{}: {}", source_path.display(), es[0])))?;
    let entry = program.modules.last().expect("entry module");
    let expected = sudoc_ir::names::test_fn_names(&entry.tests);

    let mut per_target: Vec<(String, Vec<TapLine>)> = Vec::new();
    for target in targets {
        let outcomes = run_target(&program.modules, target.as_ref())?;
        per_target.push((target.name().to_string(), outcomes));
    }

    let mut tests = Vec::new();
    for name in &expected {
        let outcomes: Vec<(String, Outcome)> = per_target
            .iter()
            .map(|(t, results)| {
                let o = results
                    .iter()
                    .find(|l| l.name == *name)
                    .map(|l| l.outcome.clone())
                    .unwrap_or(Outcome::Missing);
                (t.clone(), o)
            })
            .collect();
        let details: Vec<(String, String)> = per_target
            .iter()
            .filter_map(|(t, results)| {
                results
                    .iter()
                    .find(|l| l.name == *name)
                    .and_then(|l| l.detail.clone())
                    .map(|d| (t.clone(), d))
            })
            .collect();
        let verdict = if outcomes.iter().all(|(_, o)| *o == Outcome::Pass) {
            Verdict::Pass
        } else {
            let first = &outcomes[0].1;
            let all_same_trap = matches!(first, Outcome::Trap(_))
                && outcomes.iter().all(|(_, o)| o == first);
            match (all_same_trap, first) {
                (true, Outcome::Trap(kind)) => Verdict::ConsistentFailure(kind.clone()),
                _ => Verdict::Divergence,
            }
        };
        tests.push(TestReport { name: name.clone(), outcomes, details, verdict });
    }
    Ok(ModuleReport { module: module_name, tests })
}

fn build_dir(module: &str, target: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sudoc-harness-{}-{module}-{target}",
        std::process::id()
    ))
}

fn run_target(
    modules: &[sudoc_ir::IrModule],
    target: &dyn Backend,
) -> Result<Vec<TapLine>, HarnessError> {
    let entry_name = modules.last().expect("entry").name.clone();
    let dir = build_dir(&entry_name, target.name());
    std::fs::create_dir_all(&dir).map_err(|e| HarnessError::Build {
        target: target.name().into(),
        detail: e.to_string(),
    })?;
    let result = run_target_in(modules, target, &dir);
    if result.is_ok() {
        std::fs::remove_dir_all(&dir).ok();
    }
    result
}

/// Backend-generic: write output + runtime, run the build steps, run the
/// artifact, parse the outcome protocol.
fn run_target_in(
    modules: &[sudoc_ir::IrModule],
    target: &dyn Backend,
    dir: &Path,
) -> Result<Vec<TapLine>, HarnessError> {
    let name = target.name().to_string();
    let entry = modules.last().expect("entry module").name.clone();
    sudoc_sdk::write_output(target, modules, true, dir).map_err(|e| HarnessError::Build {
        target: name.clone(),
        detail: e.to_string(),
    })?;
    let recipe = target.test_recipe(&entry);
    for step in &recipe.build {
        let out = Command::new(&step[0])
            .args(&step[1..])
            .current_dir(dir)
            .output()
            .map_err(|e| HarnessError::Build {
                target: name.clone(),
                detail: format!("{}: {e}", step[0]),
            })?;
        if !out.status.success() {
            return Err(HarnessError::Build {
                target: name.clone(),
                detail: format!(
                    "{} failed (artifacts kept in {}):\n{}",
                    step[0],
                    dir.display(),
                    String::from_utf8_lossy(&out.stderr)
                ),
            });
        }
    }
    let output = Command::new(&recipe.run[0])
        .args(&recipe.run[1..])
        .current_dir(dir)
        .output()
        .map_err(|e| HarnessError::Run {
            target: name.clone(),
            detail: format!("{}: {e}", recipe.run[0]),
        })?;
    Ok(parse_tap(&String::from_utf8_lossy(&output.stdout)))
}

fn clip(s: &str) -> String {
    if s.len() > 300 {
        format!("{}…", &s[..300])
    } else {
        s.to_string()
    }
}

/// Render a human-readable report. Returns (text, all_green).
pub fn render(report: &ModuleReport) -> (String, bool) {
    let mut out = String::new();
    let targets: Vec<&str> = report
        .tests
        .first()
        .map(|t| t.outcomes.iter().map(|(t, _)| t.as_str()).collect())
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "== {} ({} test{}; targets: {})",
        report.module,
        report.tests.len(),
        if report.tests.len() == 1 { "" } else { "s" },
        targets.join(", ")
    );
    let mut saw_stack_overflow = false;
    for t in &report.tests {
        match &t.verdict {
            Verdict::Pass => {
                let _ = writeln!(out, "   ok        {}", t.name);
            }
            Verdict::ConsistentFailure(kind) => {
                let _ = writeln!(
                    out,
                    "   FAIL      {} — {kind} in every target (implementations agree; the test or algorithm is wrong)",
                    t.name
                );
                for (target, d) in &t.details {
                    let _ = writeln!(out, "                {target:<4} {}", clip(d));
                }
            }
            Verdict::Divergence => {
                let _ = writeln!(out, "   DIVERGED  {}", t.name);
                for (target, o) in &t.outcomes {
                    let desc = match o {
                        Outcome::Pass => "pass".to_string(),
                        Outcome::Trap(k) => {
                            if k == "StackOverflow" {
                                saw_stack_overflow = true;
                            }
                            format!("trap {k}")
                        }
                        Outcome::Missing => "no result (runner crashed?)".to_string(),
                    };
                    let detail = t
                        .details
                        .iter()
                        .find(|(dt, _)| dt == target)
                        .map(|(_, d)| format!(" — {}", clip(d)))
                        .unwrap_or_default();
                    let _ = writeln!(out, "                {target:<4} {desc}{detail}");
                }
            }
        }
    }
    let n_div = report.divergences();
    if n_div > 0 {
        let _ = writeln!(
            out,
            "   note: implementations disagreed on {n_div} test{}. If the test touches Map/Set\n   iteration, the algorithm likely depends on unspecified order (spec §12) — sort first.",
            if n_div == 1 { "" } else { "s" }
        );
        if saw_stack_overflow {
            let _ = writeln!(
                out,
                "   note: a StackOverflow divergence usually means recursion depth exceeded one\n   target's stack — not necessarily a logic bug."
            );
        }
    }
    (out, report.all_pass())
}
