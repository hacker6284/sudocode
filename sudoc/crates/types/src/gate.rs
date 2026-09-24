//! Drop functions a required predicate refuses, after typechecking.
//!
//! An empty requirement list returns the modules unchanged. The cone is the
//! `calls` and `refs` already stored on each fact; a missing fact is not
//! acceptance. Refused functions are omitted. A refused export is an error.

use std::collections::{HashMap, HashSet};

use sudoc_ir::IrModule;

use crate::termination::{self, FuncFact, FuncId, TerminationFacts, TestId};
use crate::Program;

const CHAIN_CAP: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Gated {
    pub modules: Vec<IrModule>,
    pub skips: SkipReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipReport {
    pub predicates: Vec<String>,
    /// Entry-module test function names, from the unstripped entry tests,
    /// restricted to refused tests.
    pub skipped_tests: Vec<String>,
    pub refused: Vec<RefusedItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedKind {
    Func,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedItem {
    pub module: String,
    pub name: String,
    pub kind: RefusedKind,
    pub line: u32,
    pub col: u32,
    pub predicate: String,
    pub reason: String,
    /// Set for a refused entry test. Equals that test's `skipped_tests` entry.
    pub test_fn: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConeFrame {
    pub module: String,
    pub name: String,
    pub line: u32,
    pub col: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedExport {
    pub module: String,
    pub name: String,
    pub chain: Vec<ConeFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    UnknownPredicate(String),
    RefusedExport(Vec<RefusedExport>),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::UnknownPredicate(name) => write!(f, "unknown predicate '{name}'"),
            GateError::RefusedExport(exports) => {
                for (i, exp) in exports.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "refused export `{}.{}`", exp.module, exp.name)?;
                    for frame in &exp.chain {
                        write!(
                            f,
                            "\n  {}.sudo:{}:{}: `{}` — {}",
                            frame.module, frame.line, frame.col, frame.name, frame.reason
                        )?;
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone)]
struct Mark {
    line: u32,
    col: u32,
    reason: String,
}

struct Edge {
    line: u32,
    col: u32,
    reason: String,
    callee: FuncId,
}

/// `required` empty: the modules are unchanged. Any other name than
/// `terminates` fails closed.
pub fn apply(program: &Program, required: &[&str]) -> Result<Gated, GateError> {
    if required.is_empty() {
        return Ok(identity(program));
    }
    let mut predicates = Vec::new();
    for name in required {
        if !termination::PREDICATES.contains(name) {
            return Err(GateError::UnknownPredicate((*name).to_string()));
        }
        if !predicates.iter().any(|p| p == *name) {
            predicates.push((*name).to_string());
        }
    }
    apply_terminates(program, predicates)
}

fn identity(program: &Program) -> Gated {
    Gated {
        modules: program.modules.clone(),
        skips: SkipReport {
            predicates: Vec::new(),
            skipped_tests: Vec::new(),
            refused: Vec::new(),
        },
    }
}

/// Stderr note for a successful strip. Empty when nothing was refused.
pub fn refusal_note(report: &SkipReport) -> Option<String> {
    if report.refused.is_empty() {
        return None;
    }
    let preds = report.predicates.join("`, `");
    let mut lines = vec![format!(
        "note: predicate `{preds}` refused {} function(s) for this emit (not emitted):",
        report.refused.len()
    )];
    for item in &report.refused {
        let who = match item.kind {
            RefusedKind::Func => format!("`{}`", item.name),
            RefusedKind::Test => format!("test \"{}\"", item.name),
        };
        lines.push(format!(
            "  {}.sudo:{}:{}: {who} — {}",
            item.module, item.line, item.col, item.reason
        ));
    }
    Some(lines.join("\n"))
}

fn apply_terminates(program: &Program, predicates: Vec<String>) -> Result<Gated, GateError> {
    let facts = &program.termination;
    require_facts(program);
    let (refused_funcs, refused_tests) = close(facts);
    let refused_ids: HashSet<FuncId> = refused_funcs.keys().cloned().collect();

    let mut exports = Vec::new();
    for module in &program.modules {
        for func in &module.funcs {
            if !func.export {
                continue;
            }
            let id = FuncId {
                module: module.name.clone(),
                name: func.name.clone(),
            };
            if refused_funcs.contains_key(&id) {
                exports.push(RefusedExport {
                    module: module.name.clone(),
                    name: func.name.clone(),
                    chain: export_chain(&id, facts, &refused_ids),
                });
            }
        }
    }
    if !exports.is_empty() {
        return Err(GateError::RefusedExport(exports));
    }

    let entry = program.modules.last();
    let full_names = entry
        .map(|m| sudoc_ir::names::test_fn_names(&m.tests))
        .unwrap_or_default();
    let entry_name = entry.map(|m| m.name.clone());

    let mut skipped_tests = Vec::new();
    let mut refused = Vec::new();
    for module in &program.modules {
        let is_entry = entry_name.as_deref() == Some(module.name.as_str());
        for func in &module.funcs {
            let id = FuncId {
                module: module.name.clone(),
                name: func.name.clone(),
            };
            if let Some(mark) = refused_funcs.get(&id) {
                refused.push(item_from(
                    &id.module,
                    &id.name,
                    RefusedKind::Func,
                    mark,
                    None,
                ));
            }
        }
        for (index, test) in module.tests.iter().enumerate() {
            let id = TestId {
                module: module.name.clone(),
                name: test.name.clone(),
            };
            let Some(mark) = refused_tests.get(&id) else {
                continue;
            };
            let test_fn = if is_entry {
                let name = full_names[index].clone();
                skipped_tests.push(name.clone());
                Some(name)
            } else {
                None
            };
            refused.push(item_from(
                &id.module,
                &id.name,
                RefusedKind::Test,
                mark,
                test_fn,
            ));
        }
    }

    let mut modules = Vec::with_capacity(program.modules.len());
    for module in &program.modules {
        let mut stripped = module.clone();
        stripped.funcs.retain(|func| {
            !refused_funcs.contains_key(&FuncId {
                module: module.name.clone(),
                name: func.name.clone(),
            })
        });
        stripped.tests.retain(|test| {
            !refused_tests.contains_key(&TestId {
                module: module.name.clone(),
                name: test.name.clone(),
            })
        });
        modules.push(stripped);
    }
    debug_no_dangling(&modules, &refused_ids, facts);

    if let Some(entry_module) = modules.last_mut() {
        if entry_name.as_deref() == Some(entry_module.name.as_str()) {
            let original = program.modules.last().expect("entry");
            let mut tests = Vec::new();
            for (index, test) in original.tests.iter().enumerate() {
                let id = TestId {
                    module: entry_module.name.clone(),
                    name: test.name.clone(),
                };
                if refused_tests.contains_key(&id) {
                    continue;
                }
                let mut kept = test.clone();
                kept.name = stem_of(&full_names[index]);
                tests.push(kept);
            }
            entry_module.tests = tests;
            let survivor_names: Vec<String> = original
                .tests
                .iter()
                .enumerate()
                .filter(|(_, test)| {
                    !refused_tests.contains_key(&TestId {
                        module: entry_module.name.clone(),
                        name: test.name.clone(),
                    })
                })
                .map(|(index, _)| full_names[index].clone())
                .collect();
            assert_eq!(
                sudoc_ir::names::test_fn_names(&entry_module.tests),
                survivor_names,
                "stripped entry test names drifted from the full list"
            );
        }
    }

    Ok(Gated {
        modules,
        skips: SkipReport {
            predicates,
            skipped_tests,
            refused,
        },
    })
}

fn item_from(
    module: &str,
    name: &str,
    kind: RefusedKind,
    mark: &Mark,
    test_fn: Option<String>,
) -> RefusedItem {
    RefusedItem {
        module: module.to_string(),
        name: name.to_string(),
        kind,
        line: mark.line,
        col: mark.col,
        predicate: termination::PREDICATES[0].to_string(),
        reason: mark.reason.clone(),
        test_fn,
    }
}

fn stem_of(test_fn: &str) -> String {
    test_fn
        .strip_prefix("test_")
        .unwrap_or_else(|| {
            panic!("internal error: test function name '{test_fn}' has no test_ prefix")
        })
        .to_string()
}

fn require_facts(program: &Program) {
    let facts = &program.termination;
    for module in &program.modules {
        for func in &module.funcs {
            require_func(
                facts,
                &FuncId {
                    module: module.name.clone(),
                    name: func.name.clone(),
                },
            );
        }
        for test in &module.tests {
            let id = TestId {
                module: module.name.clone(),
                name: test.name.clone(),
            };
            if !facts.tests.contains_key(&id) {
                panic!(
                    "internal error: missing termination fact for test {}.{}",
                    id.module, id.name
                );
            }
        }
    }
    for fact in facts.funcs.values().chain(facts.tests.values()) {
        for call in &fact.calls {
            require_func(facts, &call.callee);
        }
        for site in &fact.refs {
            require_func(facts, &site.callee);
        }
    }
}

fn require_func(facts: &TerminationFacts, id: &FuncId) {
    if !facts.funcs.contains_key(id) {
        panic!(
            "internal error: missing termination fact for {}.{}",
            id.module, id.name
        );
    }
}

/// Direct verdicts seed the set. Callers and ref sites close over it.
/// A direct reason is kept; a cone reason is the earliest site whose callee
/// is refused.
fn close(facts: &TerminationFacts) -> (HashMap<FuncId, Mark>, HashMap<TestId, Mark>) {
    let mut funcs: HashMap<FuncId, Mark> = HashMap::new();
    let mut tests: HashMap<TestId, Mark> = HashMap::new();
    for (id, fact) in &facts.funcs {
        if let Some(direct) = &fact.direct {
            funcs.insert(
                id.clone(),
                mark_of(direct.line, direct.col, direct.reason.clone()),
            );
        }
    }
    for (id, fact) in &facts.tests {
        if let Some(direct) = &fact.direct {
            tests.insert(
                id.clone(),
                mark_of(direct.line, direct.col, direct.reason.clone()),
            );
        }
    }
    loop {
        let refused_ids: HashSet<FuncId> = funcs.keys().cloned().collect();
        let mut changed = false;
        for (id, fact) in &facts.funcs {
            if fact.direct.is_some() {
                continue;
            }
            if let Some(mark) = earliest(fact, &refused_ids) {
                changed |= assign(&mut funcs, id, mark);
            }
        }
        for (id, fact) in &facts.tests {
            if fact.direct.is_some() {
                continue;
            }
            if let Some(mark) = earliest(fact, &refused_ids) {
                changed |= assign(&mut tests, id, mark);
            }
        }
        if !changed {
            break;
        }
    }
    (funcs, tests)
}

fn mark_of(line: u32, col: u32, reason: String) -> Mark {
    Mark { line, col, reason }
}

fn assign<K: Eq + std::hash::Hash + Clone>(map: &mut HashMap<K, Mark>, id: &K, mark: Mark) -> bool {
    match map.get(id) {
        Some(prev) if (mark.line, mark.col) >= (prev.line, prev.col) => false,
        _ => {
            map.insert(id.clone(), mark);
            true
        }
    }
}

fn earliest(fact: &FuncFact, refused: &HashSet<FuncId>) -> Option<Mark> {
    let mut best: Option<Mark> = None;
    for call in &fact.calls {
        if !refused.contains(&call.callee) {
            continue;
        }
        consider(
            &mut best,
            call.line,
            call.col,
            format!(
                "calls refused function {}.{}",
                call.callee.module, call.callee.name
            ),
        );
    }
    for site in &fact.refs {
        if !refused.contains(&site.callee) {
            continue;
        }
        consider(
            &mut best,
            site.line,
            site.col,
            format!(
                "references refused function {}.{}",
                site.callee.module, site.callee.name
            ),
        );
    }
    best
}

fn consider(best: &mut Option<Mark>, line: u32, col: u32, reason: String) {
    let replace = match best {
        None => true,
        Some(prev) => (line, col) < (prev.line, prev.col),
    };
    if replace {
        *best = Some(mark_of(line, col, reason));
    }
}

fn export_chain(
    start: &FuncId,
    facts: &TerminationFacts,
    refused: &HashSet<FuncId>,
) -> Vec<ConeFrame> {
    let mut search = ChainSearch {
        facts,
        refused,
        path: Vec::new(),
        seen: HashSet::new(),
        best: Vec::new(),
        reached: false,
    };
    search.walk(start);
    if search.best.is_empty() {
        panic!(
            "internal error: refused export {}.{} has an empty cone chain",
            start.module, start.name
        );
    }
    search.best
}

struct ChainSearch<'a> {
    facts: &'a TerminationFacts,
    refused: &'a HashSet<FuncId>,
    path: Vec<ConeFrame>,
    seen: HashSet<FuncId>,
    best: Vec<ConeFrame>,
    reached: bool,
}

impl ChainSearch<'_> {
    fn walk(&mut self, id: &FuncId) {
        if self.reached || self.path.len() >= CHAIN_CAP || !self.seen.insert(id.clone()) {
            return;
        }
        let fact = self.facts.funcs.get(id).unwrap_or_else(|| {
            panic!(
                "internal error: missing termination fact for {}.{}",
                id.module, id.name
            )
        });
        if let Some(direct) = &fact.direct {
            self.path
                .push(frame(id, direct.line, direct.col, direct.reason.clone()));
            self.best = self.path.clone();
            self.reached = true;
            self.path.pop();
            self.seen.remove(id);
            return;
        }
        let edges = edges_to_refused(fact, self.refused);
        if edges.is_empty() {
            panic!(
                "internal error: {}.{} is refused without a direct verdict or a cone edge",
                id.module, id.name
            );
        }
        for edge in &edges {
            if self.reached || self.path.len() >= CHAIN_CAP {
                break;
            }
            self.path
                .push(frame(id, edge.line, edge.col, edge.reason.clone()));
            if self.path.len() == CHAIN_CAP && self.best.len() < CHAIN_CAP {
                self.best = self.path.clone();
            }
            let callee = edge.callee.clone();
            self.walk(&callee);
            self.path.pop();
        }
        self.seen.remove(id);
    }
}

fn frame(id: &FuncId, line: u32, col: u32, reason: String) -> ConeFrame {
    ConeFrame {
        module: id.module.clone(),
        name: id.name.clone(),
        line,
        col,
        reason,
    }
}

fn edges_to_refused(fact: &FuncFact, refused: &HashSet<FuncId>) -> Vec<Edge> {
    let mut edges = Vec::new();
    for call in &fact.calls {
        if !refused.contains(&call.callee) {
            continue;
        }
        edges.push(Edge {
            line: call.line,
            col: call.col,
            reason: format!(
                "calls refused function {}.{}",
                call.callee.module, call.callee.name
            ),
            callee: call.callee.clone(),
        });
    }
    for site in &fact.refs {
        if !refused.contains(&site.callee) {
            continue;
        }
        edges.push(Edge {
            line: site.line,
            col: site.col,
            reason: format!(
                "references refused function {}.{}",
                site.callee.module, site.callee.name
            ),
            callee: site.callee.clone(),
        });
    }
    edges.sort_by_key(|edge| (edge.line, edge.col));
    edges
}

fn debug_no_dangling(modules: &[IrModule], removed: &HashSet<FuncId>, facts: &TerminationFacts) {
    for module in modules {
        for func in &module.funcs {
            let id = FuncId {
                module: module.name.clone(),
                name: func.name.clone(),
            };
            if let Some(fact) = facts.funcs.get(&id) {
                debug_fact(&id.module, &id.name, fact, removed);
            }
        }
        for test in &module.tests {
            let id = TestId {
                module: module.name.clone(),
                name: test.name.clone(),
            };
            if let Some(fact) = facts.tests.get(&id) {
                debug_fact(&id.module, &id.name, fact, removed);
            }
        }
    }
}

fn debug_fact(module: &str, name: &str, fact: &FuncFact, removed: &HashSet<FuncId>) {
    for call in &fact.calls {
        debug_assert!(
            !removed.contains(&call.callee),
            "cone left a call from {module}.{name} to stripped {}.{}",
            call.callee.module,
            call.callee.name
        );
    }
    for site in &fact.refs {
        debug_assert!(
            !removed.contains(&site.callee),
            "cone left a reference from {module}.{name} to stripped {}.{}",
            site.callee.module,
            site.callee.name
        );
    }
}

