//! The `terminates` predicate: measure honesty and a direct verdict.
//!
//! `decreases` is not stored on the IR. A measure this module cannot prove is
//! a `TypeError`. An unproved body is a `Refusal` on [`FuncFact::direct`].

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};

use sudoc_ir::never_written::written_locals;
use sudoc_ir::{
    BinaryOp, Builtin, IrExpr, IrExprKind, IrFunc, IrModule, IrPattern, IrStmt, Place, Ty, UnaryOp,
};

use crate::TypeError;

pub const PREDICATES: &[&str] = &["terminates"];

pub(crate) const MEASURE_FUNC: &str =
    "decreases measure must be an int parameter, or .length / .size of a parameter";
pub(crate) const MEASURE_WHILE: &str =
    "decreases measure must be an int local or parameter, or .length / .size of a local or parameter";
const DECREASE_FAIL: &str = "decreases measure does not decrease";
const REASON_INDIRECT: &str = "indirect call; terminates cannot see the callee";
const REASON_WHILE: &str = "while has no decreases measure";
const REASON_STRUCT: &str = "recursive call is not structural descent";
const STRUCT_FUEL: u64 = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FuncId {
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TestId {
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub line: u32,
    pub col: u32,
    pub predicate: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Direct,
    ResolvedValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallSite {
    pub line: u32,
    pub col: u32,
    pub callee: FuncId,
    pub kind: CallKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefSite {
    pub line: u32,
    pub col: u32,
    pub callee: FuncId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndirectSite {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopSite {
    pub line: u32,
    pub col: u32,
    pub decreases: Option<IrExpr>,
}

/// One pre-hoist call, in typecheck order. Hoist temps are not reclassified.
#[derive(Debug, Clone, PartialEq)]
pub enum WalkCall {
    Recorded(CallSite),
    Indirect(IndirectSite),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncSites {
    pub line: u32,
    pub col: u32,
    pub export: bool,
    pub decreases: Option<IrExpr>,
    pub loops: Vec<LoopSite>,
    pub calls: Vec<CallSite>,
    pub refs: Vec<RefSite>,
    pub indirect: Vec<IndirectSite>,
    /// `calls` and `indirect` in the order they were typechecked.
    pub slots: Vec<WalkCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncFact {
    pub line: u32,
    pub col: u32,
    pub export: bool,
    /// `None` when the direct rules accept the body.
    pub direct: Option<Refusal>,
    pub calls: Vec<CallSite>,
    pub refs: Vec<RefSite>,
    pub loops: Vec<LoopSite>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TerminationFacts {
    pub funcs: BTreeMap<FuncId, FuncFact>,
    pub tests: BTreeMap<TestId, FuncFact>,
}

pub(crate) fn func_id(module: &str, call_name: &str) -> FuncId {
    match call_name.split_once('.') {
        Some((m, n)) => FuncId {
            module: m.to_string(),
            name: n.to_string(),
        },
        None => FuncId {
            module: module.to_string(),
            name: call_name.to_string(),
        },
    }
}

#[derive(Default)]
struct FnBind {
    func: Option<FuncId>,
    assigns: u32,
    poisoned: bool,
}

/// Locals assigned once from a `FuncRef` or a copy of one, and not written again.
pub(crate) fn resolved_func_locals(
    module: &str,
    body: &[IrStmt],
    inout_params: &HashMap<String, Vec<bool>>,
) -> HashMap<String, FuncId> {
    let mut info: HashMap<String, FnBind> = HashMap::new();
    walk_resolve(body, &mut info, module, false, inout_params);
    info.into_iter()
        .filter_map(|(n, b)| {
            if b.assigns == 1 && !b.poisoned {
                b.func.map(|f| (n, f))
            } else {
                None
            }
        })
        .collect()
}

fn poison(info: &mut HashMap<String, FnBind>, name: &str) {
    let e = info.entry(name.to_string()).or_default();
    e.poisoned = true;
    e.func = None;
}

fn place_root_name(p: &Place) -> &str {
    match p {
        Place::Var(n) => n,
        Place::Field { base, .. } | Place::Index { base, .. } => place_root_name(base),
    }
}

fn func_ref_of(value: &IrExpr, info: &HashMap<String, FnBind>, module: &str) -> Option<FuncId> {
    match &value.kind {
        IrExprKind::FuncRef(name) => Some(func_id(module, name)),
        IrExprKind::Local(n) => {
            let b = info.get(n)?;
            if b.assigns == 1 && !b.poisoned {
                b.func.clone()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn poison_expr(
    e: &IrExpr,
    info: &mut HashMap<String, FnBind>,
    module: &str,
    inout_params: &HashMap<String, Vec<bool>>,
) {
    match &e.kind {
        IrExprKind::CallFunc { name, args } => {
            for a in args {
                poison_expr(a, info, module, inout_params);
            }
            if let Some(flags) = inout_params.get(name) {
                for (a, inout) in args.iter().zip(flags) {
                    if *inout {
                        if let Some(r) = expr_local(a) {
                            poison(info, r);
                        }
                    }
                }
            }
        }
        IrExprKind::CallValue { callee, args } => {
            poison_expr(callee, info, module, inout_params);
            for a in args {
                poison_expr(a, info, module, inout_params);
            }
        }
        IrExprKind::MutBuiltin { recv, args, .. } => {
            poison(info, place_root_name(recv));
            poison_place(recv, info, module, inout_params);
            for a in args {
                poison_expr(a, info, module, inout_params);
            }
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            for x in xs {
                poison_expr(x, info, module, inout_params);
            }
        }
        IrExprKind::GetField { recv, .. } => poison_expr(recv, info, module, inout_params),
        IrExprKind::Index { recv, index } => {
            poison_expr(recv, info, module, inout_params);
            poison_expr(index, info, module, inout_params);
        }
        IrExprKind::Unary { operand, .. } => poison_expr(operand, info, module, inout_params),
        IrExprKind::Binary { lhs, rhs, .. } => {
            poison_expr(lhs, info, module, inout_params);
            poison_expr(rhs, info, module, inout_params);
        }
        _ => {}
    }
}

fn poison_place(
    p: &Place,
    info: &mut HashMap<String, FnBind>,
    module: &str,
    inout_params: &HashMap<String, Vec<bool>>,
) {
    match p {
        Place::Var(_) => {}
        Place::Index { base, index, .. } => {
            poison_place(base, info, module, inout_params);
            poison_expr(index, info, module, inout_params);
        }
        Place::Field { base, .. } => poison_place(base, info, module, inout_params),
    }
}

fn expr_local(e: &IrExpr) -> Option<&str> {
    match &e.kind {
        IrExprKind::Local(n) => Some(n),
        IrExprKind::GetField { recv, .. } | IrExprKind::Index { recv, .. } => expr_local(recv),
        _ => None,
    }
}

fn walk_resolve(
    stmts: &[IrStmt],
    info: &mut HashMap<String, FnBind>,
    module: &str,
    in_loop: bool,
    inout_params: &HashMap<String, Vec<bool>>,
) {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                poison_expr(value, info, module, inout_params);
                poison_place(target, info, module, inout_params);
                match target {
                    Place::Var(n) => {
                        if in_loop {
                            poison(info, n);
                        } else if let Some(id) = func_ref_of(value, info, module) {
                            let e = info.entry(n.clone()).or_default();
                            if e.assigns == 0 && !e.poisoned {
                                e.func = Some(id);
                                e.assigns = 1;
                            } else {
                                e.poisoned = true;
                                e.func = None;
                                e.assigns = e.assigns.saturating_add(1);
                            }
                        } else {
                            poison(info, n);
                        }
                    }
                    other => poison(info, place_root_name(other)),
                }
            }
            IrStmt::TupleAssign { targets, value, .. } => {
                poison_expr(value, info, module, inout_params);
                for t in targets {
                    poison(info, t);
                }
            }
            IrStmt::Expr(e) | IrStmt::Return(Some(e)) => poison_expr(e, info, module, inout_params),
            IrStmt::Assert { cond, .. } => poison_expr(cond, info, module, inout_params),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    poison_expr(c, info, module, inout_params);
                    walk_resolve(b, info, module, in_loop, inout_params);
                }
                if let Some(b) = else_block {
                    walk_resolve(b, info, module, in_loop, inout_params);
                }
            }
            IrStmt::While { cond, body } => {
                poison_expr(cond, info, module, inout_params);
                walk_resolve(body, info, module, true, inout_params);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                poison_expr(from, info, module, inout_params);
                poison_expr(to, info, module, inout_params);
                walk_resolve(body, info, module, true, inout_params);
            }
            IrStmt::ForIn { iter, body, .. } => {
                poison_expr(iter, info, module, inout_params);
                walk_resolve(body, info, module, true, inout_params);
            }
            IrStmt::Match { scrutinee, arms } => {
                poison_expr(scrutinee, info, module, inout_params);
                for a in arms {
                    walk_resolve(&a.body, info, module, in_loop, inout_params);
                }
            }
            IrStmt::ExpectTrap { body, .. } => {
                walk_resolve(body, info, module, in_loop, inout_params)
            }
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
}

pub(crate) fn inout_map_from_modules(modules: &[IrModule]) -> HashMap<String, Vec<bool>> {
    let mut m = HashMap::new();
    for module in modules {
        for f in &module.funcs {
            let flags: Vec<bool> = f.params.iter().map(|p| p.inout).collect();
            m.insert(f.name.clone(), flags.clone());
            m.insert(format!("{}.{}", module.name, f.name), flags);
        }
    }
    m
}

/// Honesty, then a direct verdict for every function and test.
/// A missing site entry is a compiler bug.
pub fn analyze(
    modules: &[IrModule],
    func_sites: &BTreeMap<FuncId, FuncSites>,
    test_sites: &BTreeMap<TestId, FuncSites>,
) -> Result<TerminationFacts, Vec<TypeError>> {
    let mut errors = Vec::new();
    let mut ids = Vec::new();
    let mut edges: HashMap<FuncId, Vec<FuncId>> = HashMap::new();
    for module in modules {
        for f in &module.funcs {
            let id = FuncId {
                module: module.name.clone(),
                name: f.name.clone(),
            };
            let sites = require_func_sites(func_sites, &id);
            edges.insert(
                id.clone(),
                sites.calls.iter().map(|c| c.callee.clone()).collect(),
            );
            ids.push(id);
        }
    }
    let sccs = strongly_connected(&ids, &edges);
    let mut nontrivial: HashMap<FuncId, Vec<FuncId>> = HashMap::new();
    for comp in &sccs {
        if comp_nontrivial(comp, &edges) {
            for id in comp {
                nontrivial.insert(id.clone(), comp.clone());
            }
        }
    }

    let inout_params = inout_map_from_modules(modules);
    for module in modules {
        for f in &module.funcs {
            let id = FuncId {
                module: module.name.clone(),
                name: f.name.clone(),
            };
            let sites = require_func_sites(func_sites, &id);
            if sites.decreases.is_some() {
                if let Some(comp) = nontrivial.get(&id) {
                    errors.extend(prove_function_measure(
                        modules,
                        module,
                        f,
                        sites,
                        comp,
                        func_sites,
                        &inout_params,
                    ));
                }
            }
            errors.extend(prove_whiles(modules, module, &f.body, sites, &inout_params));
        }
        for t in &module.tests {
            let tid = TestId {
                module: module.name.clone(),
                name: t.name.clone(),
            };
            let sites = test_sites.get(&tid).unwrap_or_else(|| {
                panic!(
                    "internal error: missing termination sites for test {}.{}",
                    tid.module, tid.name
                )
            });
            errors.extend(prove_whiles(modules, module, &t.body, sites, &inout_params));
        }
    }
    if !errors.is_empty() {
        errors.sort_by_key(|e| (e.line, e.col));
        return Err(errors);
    }

    let mut struct_ok: HashMap<FuncId, bool> = HashMap::new();
    for comp in &sccs {
        if !comp_nontrivial(comp, &edges) {
            continue;
        }
        let ok = scc_structural(modules, comp, func_sites);
        for id in comp {
            struct_ok.insert(id.clone(), ok);
        }
    }

    let mut facts = TerminationFacts::default();
    for module in modules {
        for f in &module.funcs {
            let id = FuncId {
                module: module.name.clone(),
                name: f.name.clone(),
            };
            let sites = require_func_sites(func_sites, &id);
            let comp = nontrivial.get(&id);
            let ok = if comp.is_some() {
                *struct_ok.get(&id).unwrap_or_else(|| {
                    panic!(
                        "internal error: missing structural result for {}.{}",
                        id.module, id.name
                    )
                })
            } else {
                true
            };
            let direct = direct_verdict(sites, comp, ok);
            facts.funcs.insert(id, fact_from(sites, direct));
        }
        for t in &module.tests {
            let tid = TestId {
                module: module.name.clone(),
                name: t.name.clone(),
            };
            let sites = &test_sites[&tid];
            let direct = direct_verdict(sites, None, true);
            facts.tests.insert(tid, fact_from(sites, direct));
        }
    }
    Ok(facts)
}

fn require_func_sites<'a>(map: &'a BTreeMap<FuncId, FuncSites>, id: &FuncId) -> &'a FuncSites {
    map.get(id).unwrap_or_else(|| {
        panic!(
            "internal error: missing termination sites for {}.{}",
            id.module, id.name
        )
    })
}

fn fact_from(sites: &FuncSites, direct: Option<Refusal>) -> FuncFact {
    FuncFact {
        line: sites.line,
        col: sites.col,
        export: sites.export,
        direct,
        calls: sites.calls.clone(),
        refs: sites.refs.clone(),
        loops: sites.loops.clone(),
    }
}

fn direct_verdict(
    sites: &FuncSites,
    comp: Option<&Vec<FuncId>>,
    struct_ok: bool,
) -> Option<Refusal> {
    let mut fails: Vec<(u32, u32, &str)> = Vec::new();
    for ind in &sites.indirect {
        fails.push((ind.line, ind.col, REASON_INDIRECT));
    }
    for loop_site in &sites.loops {
        if loop_site.decreases.is_none() {
            fails.push((loop_site.line, loop_site.col, REASON_WHILE));
        }
    }
    if sites.decreases.is_none() {
        if let Some(comp) = comp {
            if !struct_ok {
                let scc: HashSet<&FuncId> = comp.iter().collect();
                if let Some(c) = sites.calls.iter().find(|c| scc.contains(&c.callee)) {
                    fails.push((c.line, c.col, REASON_STRUCT));
                } else {
                    fails.push((sites.line, sites.col, REASON_STRUCT));
                }
            }
        }
    }
    fails.sort_by_key(|f| (f.0, f.1));
    let (line, col, reason) = *fails.first()?;
    Some(Refusal {
        line,
        col,
        predicate: PREDICATES[0],
        reason: reason.to_string(),
    })
}

fn comp_nontrivial(comp: &[FuncId], edges: &HashMap<FuncId, Vec<FuncId>>) -> bool {
    if comp.len() > 1 {
        return true;
    }
    let Some(id) = comp.first() else {
        return false;
    };
    edges.get(id).is_some_and(|es| es.iter().any(|e| e == id))
}

struct Tarjan<'a> {
    edges: &'a HashMap<FuncId, Vec<FuncId>>,
    known: HashSet<&'a FuncId>,
    index: u32,
    indices: HashMap<FuncId, u32>,
    low: HashMap<FuncId, u32>,
    stack: Vec<FuncId>,
    on_stack: HashSet<FuncId>,
    comps: Vec<Vec<FuncId>>,
}

impl Tarjan<'_> {
    fn visit(&mut self, v: &FuncId) {
        let idx = self.index;
        self.indices.insert(v.clone(), idx);
        self.low.insert(v.clone(), idx);
        self.index += 1;
        self.stack.push(v.clone());
        self.on_stack.insert(v.clone());
        if let Some(es) = self.edges.get(v) {
            for w in es {
                if !self.known.contains(w) {
                    continue;
                }
                if !self.indices.contains_key(w) {
                    self.visit(w);
                    let lw = self.low[w];
                    if lw < self.low[v] {
                        self.low.insert(v.clone(), lw);
                    }
                } else if self.on_stack.contains(w) {
                    let iw = self.indices[w];
                    if iw < self.low[v] {
                        self.low.insert(v.clone(), iw);
                    }
                }
            }
        }
        if self.low[v] == self.indices[v] {
            let mut comp = Vec::new();
            loop {
                let w = self.stack.pop().expect("tarjan stack");
                self.on_stack.remove(&w);
                let done = w == *v;
                comp.push(w);
                if done {
                    break;
                }
            }
            self.comps.push(comp);
        }
    }
}

fn strongly_connected(nodes: &[FuncId], edges: &HashMap<FuncId, Vec<FuncId>>) -> Vec<Vec<FuncId>> {
    let mut t = Tarjan {
        edges,
        known: nodes.iter().collect(),
        index: 0,
        indices: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        comps: Vec::new(),
    };
    for n in nodes {
        if !t.indices.contains_key(n) {
            t.visit(n);
        }
    }
    t.comps
}

fn find_func<'a>(modules: &'a [IrModule], id: &FuncId) -> &'a IrFunc {
    modules
        .iter()
        .find(|m| m.name == id.module)
        .and_then(|m| m.func(&id.name))
        .unwrap_or_else(|| panic!("internal error: missing function {}.{}", id.module, id.name))
}

fn module_of<'a>(modules: &'a [IrModule], name: &str) -> &'a IrModule {
    modules
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("internal error: missing module {name}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Abs {
    Rel { delta: i64 },
    Top,
}

fn join_abs(a: Abs, b: Abs) -> Abs {
    match (a, b) {
        (Abs::Rel { delta: d1 }, Abs::Rel { delta: d2 }) if d1 == d2 => Abs::Rel { delta: d1 },
        _ => Abs::Top,
    }
}

/// Keep a snapshot only when every fallthrough path saved the same delta.
fn join_copies(maps: &[HashMap<String, Abs>]) -> HashMap<String, Abs> {
    let Some(first) = maps.first() else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (k, v) in first {
        if maps.iter().all(|m| m.get(k) == Some(v)) {
            out.insert(k.clone(), *v);
        }
    }
    out
}

fn apply_delta(state: Abs, change: i64) -> Abs {
    match state {
        Abs::Rel { delta } => match delta.checked_add(change) {
            Some(d) => Abs::Rel { delta: d },
            None => Abs::Top,
        },
        Abs::Top => Abs::Top,
    }
}

#[derive(Clone, Debug)]
struct MeasureRoot {
    local: String,
    kind: RootKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    Int,
    Length,
    Size,
}

fn measure_root(expr: &IrExpr) -> MeasureRoot {
    match &expr.kind {
        IrExprKind::Local(n) => MeasureRoot {
            local: n.clone(),
            kind: RootKind::Int,
        },
        IrExprKind::Builtin { builtin, args } => {
            let local = match args.as_slice() {
                [IrExpr {
                    kind: IrExprKind::Local(n),
                    ..
                }] => n.clone(),
                _ => panic!("internal error: decreases measure was not a single local"),
            };
            let kind = match builtin {
                Builtin::ListLength => RootKind::Length,
                Builtin::MapSize | Builtin::SetSize => RootKind::Size,
                _ => panic!("internal error: decreases measure builtin is not a length or size"),
            };
            MeasureRoot { local, kind }
        }
        _ => panic!("internal error: decreases measure has a non-measure shape"),
    }
}

fn int_lit(e: &IrExpr) -> Option<i64> {
    match &e.kind {
        IrExprKind::Int(k) => Some(*k),
        _ => None,
    }
}

fn neg_lit(e: &IrExpr) -> Option<i64> {
    match &e.kind {
        IrExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => int_lit(operand),
        _ => None,
    }
}

/// Change in the root relative to its current value, for `n - k` / `n + (-k)`.
fn int_delta(root: &str, value: &IrExpr) -> Option<i64> {
    let IrExprKind::Binary { op, lhs, rhs } = &value.kind else {
        return None;
    };
    let IrExprKind::Local(n) = &lhs.kind else {
        return None;
    };
    if n != root {
        return None;
    }
    match op {
        BinaryOp::Sub => {
            let k = int_lit(rhs)?;
            if k >= 1 {
                Some(-k)
            } else {
                None
            }
        }
        BinaryOp::Add => {
            if let Some(k) = int_lit(rhs) {
                if k <= -1 {
                    return Some(k);
                }
            }
            if let Some(k) = neg_lit(rhs) {
                if k >= 1 {
                    return Some(-k);
                }
            }
            None
        }
        _ => None,
    }
}

struct Flow<'a> {
    modules: &'a [IrModule],
    module_name: &'a str,
    root: MeasureRoot,
    inout_params: &'a HashMap<String, Vec<bool>>,
    while_mode: bool,
    enter_loops: bool,
    backs: Vec<Abs>,
    /// Value of the measure root captured by `tmp = root` (hoist temps included).
    copies: HashMap<String, Abs>,
    /// Set while probing a nested loop's back-edges. Obligations are not recorded.
    skip_obligations: bool,
    /// Back-edges of the nested loop currently being probed.
    probing_backs: Option<Vec<Abs>>,
    oblig: Option<Oblig<'a>>,
}

struct Oblig<'a> {
    scc: &'a HashSet<FuncId>,
    sites: &'a BTreeMap<FuncId, FuncSites>,
    slots: &'a [WalkCall],
    idx: usize,
    errors: Vec<TypeError>,
}

fn prove_whiles(
    modules: &[IrModule],
    module: &IrModule,
    body: &[IrStmt],
    sites: &FuncSites,
    inout_params: &HashMap<String, Vec<bool>>,
) -> Vec<TypeError> {
    let mut bodies = Vec::new();
    collect_whiles(body, &mut bodies);
    if bodies.len() != sites.loops.len() {
        panic!(
            "internal error: while sites ({}) != post-hoist whiles ({}) in {}.{}",
            sites.loops.len(),
            bodies.len(),
            module.name,
            sites.line
        );
    }
    let mut errors = Vec::new();
    for (body, loop_site) in bodies.into_iter().zip(&sites.loops) {
        let Some(measure) = &loop_site.decreases else {
            continue;
        };
        let root = measure_root(measure);
        if !prove_one_while(modules, &module.name, body, root, inout_params) {
            errors.push(TypeError {
                line: loop_site.line,
                col: loop_site.col,
                msg: DECREASE_FAIL.to_string(),
            });
        }
    }
    errors
}

fn collect_whiles<'a>(stmts: &'a [IrStmt], out: &mut Vec<&'a [IrStmt]>) {
    for s in stmts {
        match s {
            IrStmt::While { body, .. } => {
                out.push(body);
                collect_whiles(body, out);
            }
            IrStmt::If { arms, else_block } => {
                for (_, b) in arms {
                    collect_whiles(b, out);
                }
                if let Some(b) = else_block {
                    collect_whiles(b, out);
                }
            }
            IrStmt::ForRange { body, .. } | IrStmt::ForIn { body, .. } => collect_whiles(body, out),
            IrStmt::Match { arms, .. } => {
                for a in arms {
                    collect_whiles(&a.body, out);
                }
            }
            IrStmt::ExpectTrap { body, .. } => collect_whiles(body, out),
            _ => {}
        }
    }
}

fn prove_one_while(
    modules: &[IrModule],
    module_name: &str,
    body: &[IrStmt],
    root: MeasureRoot,
    inout_params: &HashMap<String, Vec<bool>>,
) -> bool {
    let mut flow = Flow {
        modules,
        module_name,
        root,
        inout_params,
        while_mode: true,
        enter_loops: false,
        backs: Vec::new(),
        copies: HashMap::new(),
        skip_obligations: false,
        probing_backs: None,
        oblig: None,
    };
    let fall = flow.flow_stmts(body, Some(Abs::Rel { delta: 0 }));
    if let Some(st) = fall {
        flow.backs.push(st);
    }
    flow.backs
        .iter()
        .all(|b| matches!(b, Abs::Rel { delta } if *delta < 0))
}

fn prove_function_measure(
    modules: &[IrModule],
    module: &IrModule,
    func: &IrFunc,
    sites: &FuncSites,
    comp: &[FuncId],
    all_sites: &BTreeMap<FuncId, FuncSites>,
    inout_params: &HashMap<String, Vec<bool>>,
) -> Vec<TypeError> {
    let measure = sites.decreases.as_ref().expect("caller checks decreases");
    let root = measure_root(measure);
    let scc: HashSet<FuncId> = comp.iter().cloned().collect();
    let mut flow = Flow {
        modules,
        module_name: &module.name,
        root,
        inout_params,
        while_mode: false,
        enter_loops: true,
        backs: Vec::new(),
        copies: HashMap::new(),
        skip_obligations: false,
        probing_backs: None,
        oblig: Some(Oblig {
            scc: &scc,
            sites: all_sites,
            slots: &sites.slots,
            idx: 0,
            errors: Vec::new(),
        }),
    };
    let _ = flow.flow_stmts(&func.body, Some(Abs::Rel { delta: 0 }));
    let oblig = flow.oblig.as_mut().unwrap();
    if oblig.idx != oblig.slots.len() {
        panic!(
            "internal error: call sites ({}) != walked calls ({}) in {}.{}",
            oblig.slots.len(),
            oblig.idx,
            module.name,
            func.name
        );
    }
    std::mem::take(&mut oblig.errors)
}

impl Flow<'_> {
    fn flow_stmts(&mut self, stmts: &[IrStmt], mut state: Option<Abs>) -> Option<Abs> {
        for s in stmts {
            let reachable = state.is_some();
            let st = state.unwrap_or(Abs::Top);
            let next = self.flow_stmt(s, st, reachable);
            state = if reachable { next } else { None };
        }
        state
    }

    fn flow_stmt(&mut self, s: &IrStmt, state: Abs, reachable: bool) -> Option<Abs> {
        match s {
            IrStmt::Assign { target, value, .. } => {
                Some(self.flow_assign(target, value, state, reachable))
            }
            IrStmt::TupleAssign { targets, value, .. } => {
                let state = self.effects(value, state, reachable);
                if targets.iter().any(|t| t == &self.root.local) {
                    Some(Abs::Top)
                } else {
                    Some(state)
                }
            }
            IrStmt::Expr(e) => Some(self.effects(e, state, reachable)),
            IrStmt::Return(e) => {
                if let Some(e) = e {
                    let _ = self.effects(e, state, reachable);
                }
                None
            }
            IrStmt::Assert { cond, .. } => Some(self.effects(cond, state, reachable)),
            IrStmt::Skip => Some(state),
            IrStmt::Break => None,
            IrStmt::Continue => {
                if reachable {
                    if let Some(backs) = &mut self.probing_backs {
                        backs.push(state);
                    } else if self.while_mode {
                        self.backs.push(state);
                    }
                }
                None
            }
            IrStmt::If { arms, else_block } => {
                self.flow_if(arms, else_block.as_deref(), state, reachable)
            }
            IrStmt::While { cond, body } => self.flow_loop(Some(cond), body, state, reachable),
            IrStmt::ForRange { from, to, body, .. } => {
                let state = self.effects(from, state, reachable);
                let state = self.effects(to, state, reachable);
                self.flow_loop(None, body, state, reachable)
            }
            IrStmt::ForIn { iter, body, .. } => {
                let state = self.effects(iter, state, reachable);
                self.flow_loop(None, body, state, reachable)
            }
            IrStmt::Match { scrutinee, arms } => {
                let state = if writes_expr(scrutinee, &self.root, self.inout_params) {
                    let _ = self.effects(scrutinee, state, reachable);
                    Abs::Top
                } else {
                    self.effects(scrutinee, state, reachable)
                };
                let mut acc: Option<Abs> = None;
                let mut any = false;
                let incoming = self.copies.clone();
                let mut fall_copies = Vec::new();
                let enter = if reachable { Some(state) } else { None };
                for arm in arms {
                    self.copies = incoming.clone();
                    if let Some(s) = self.flow_stmts(&arm.body, enter) {
                        any = true;
                        fall_copies.push(self.copies.clone());
                        acc = Some(match acc {
                            None => s,
                            Some(p) => join_abs(p, s),
                        });
                    }
                }
                self.copies = if any {
                    join_copies(&fall_copies)
                } else {
                    incoming
                };
                if any {
                    acc
                } else {
                    None
                }
            }
            IrStmt::ExpectTrap { body, .. } => {
                let _ = self.flow_stmts(body, if reachable { Some(state) } else { None });
                if writes_stmts(body, &self.root, self.inout_params) {
                    Some(Abs::Top)
                } else {
                    Some(state)
                }
            }
        }
    }

    fn flow_if(
        &mut self,
        arms: &[(IrExpr, Vec<IrStmt>)],
        else_block: Option<&[IrStmt]>,
        state: Abs,
        reachable: bool,
    ) -> Option<Abs> {
        let cond_writes = arms
            .iter()
            .any(|(c, _)| writes_expr(c, &self.root, self.inout_params));
        let incoming = self.copies.clone();
        let mut acc: Option<Abs> = None;
        let mut any = false;
        let mut fall_copies = Vec::new();
        for (cond, arm) in arms {
            self.copies = incoming.clone();
            let st = if writes_expr(cond, &self.root, self.inout_params) {
                let _ = self.effects(cond, state, reachable);
                Abs::Top
            } else {
                self.effects(cond, state, reachable)
            };
            if let Some(s) = self.flow_stmts(arm, if reachable { Some(st) } else { None }) {
                any = true;
                fall_copies.push(self.copies.clone());
                acc = Some(match acc {
                    None => s,
                    Some(p) => join_abs(p, s),
                });
            }
        }
        let else_in = if cond_writes { Abs::Top } else { state };
        match else_block {
            None => {
                any = true;
                fall_copies.push(incoming.clone());
                acc = Some(match acc {
                    None => else_in,
                    Some(p) => join_abs(p, else_in),
                });
            }
            Some(b) => {
                self.copies = incoming.clone();
                if let Some(s) = self.flow_stmts(b, if reachable { Some(else_in) } else { None }) {
                    any = true;
                    fall_copies.push(self.copies.clone());
                    acc = Some(match acc {
                        None => s,
                        Some(p) => join_abs(p, s),
                    });
                }
            }
        }
        self.copies = if any {
            join_copies(&fall_copies)
        } else {
            incoming
        };
        if any {
            acc
        } else {
            None
        }
    }

    fn flow_loop(
        &mut self,
        cond: Option<&IrExpr>,
        body: &[IrStmt],
        state: Abs,
        reachable: bool,
    ) -> Option<Abs> {
        let mut state = state;
        if let Some(cond) = cond {
            state = self.effects(cond, state, reachable);
        }
        let writes = cond.is_some_and(|c| writes_expr(c, &self.root, self.inout_params))
            || writes_stmts(body, &self.root, self.inout_params);
        if !self.enter_loops {
            return Some(if writes { Abs::Top } else { state });
        }
        if !reachable {
            let _ = self.flow_stmts(body, None);
            return Some(if writes { Abs::Top } else { state });
        }
        let saved_copies = self.copies.clone();
        let trust = !writes || self.backedges_decrease(body, state);
        let enter = if trust { state } else { Abs::Top };
        let _ = self.flow_stmts(body, Some(enter));
        if writes {
            self.copies = saved_copies;
            Some(Abs::Top)
        } else {
            Some(state)
        }
    }

    fn backedges_decrease(&mut self, body: &[IrStmt], state: Abs) -> bool {
        let saved_copies = self.copies.clone();
        let saved_idx = self.oblig.as_ref().map(|o| o.idx);
        let saved_errs = self.oblig.as_ref().map(|o| o.errors.len());
        let saved_backs = self.backs.len();
        let saved_probe = self.probing_backs.take();
        let saved_skip = self.skip_obligations;
        self.probing_backs = Some(Vec::new());
        self.skip_obligations = true;
        let fall = self.flow_stmts(body, Some(state));
        let mut edges = self.probing_backs.take().unwrap_or_default();
        if let Some(st) = fall {
            edges.push(st);
        }
        self.copies = saved_copies;
        self.skip_obligations = saved_skip;
        self.probing_backs = saved_probe;
        self.backs.truncate(saved_backs);
        if let Some(o) = self.oblig.as_mut() {
            if let Some(idx) = saved_idx {
                o.idx = idx;
            }
            if let Some(n) = saved_errs {
                o.errors.truncate(n);
            }
        }
        edges
            .iter()
            .all(|b| matches!(b, Abs::Rel { delta } if *delta < 0))
    }

    fn flow_assign(&mut self, target: &Place, value: &IrExpr, state: Abs, reachable: bool) -> Abs {
        let state = self.effects(value, state, reachable);
        let state = self.effects_place(target, state, reachable);
        if let Place::Var(n) = target {
            if n != &self.root.local {
                if let Some(saved) = self.snapshot(value, state) {
                    self.copies.insert(n.clone(), saved);
                } else {
                    self.copies.remove(n);
                }
            }
        }
        if place_root_name(target) != self.root.local {
            return state;
        }
        if self.root.kind == RootKind::Int {
            if let Place::Var(n) = target {
                if n == &self.root.local {
                    if let Some(change) = int_delta(n, value) {
                        return apply_delta(state, change);
                    }
                }
            }
        }
        Abs::Top
    }

    fn snapshot(&self, value: &IrExpr, state: Abs) -> Option<Abs> {
        let IrExprKind::Local(n) = &value.kind else {
            return None;
        };
        if n == &self.root.local {
            return Some(state);
        }
        self.copies.get(n).copied()
    }

    fn effects(&mut self, e: &IrExpr, state: Abs, reachable: bool) -> Abs {
        match &e.kind {
            IrExprKind::CallFunc { name, args } => {
                let mut state = state;
                for a in args {
                    state = self.effects(a, state, reachable);
                }
                self.on_call(name, args, state, reachable)
            }
            IrExprKind::CallValue { callee, args } => {
                let mut state = self.effects(callee, state, reachable);
                for a in args {
                    state = self.effects(a, state, reachable);
                }
                self.on_value_call(callee, args, state, reachable)
            }
            IrExprKind::MutBuiltin {
                builtin,
                recv,
                args,
                ..
            } => {
                let mut state = state;
                for a in args {
                    state = self.effects(a, state, reachable);
                }
                state = self.effects_place(recv, state, reachable);
                self.mut_effect(*builtin, recv, state)
            }
            IrExprKind::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                lhs,
                rhs,
            } => {
                let l = self.effects(lhs, state, reachable);
                let r = self.effects(rhs, l, reachable);
                join_abs(l, r)
            }
            IrExprKind::Binary { lhs, rhs, .. } => {
                let state = self.effects(lhs, state, reachable);
                self.effects(rhs, state, reachable)
            }
            IrExprKind::Unary { operand, .. } => self.effects(operand, state, reachable),
            IrExprKind::List(xs)
            | IrExprKind::Tuple(xs)
            | IrExprKind::NewRecord { args: xs, .. }
            | IrExprKind::NewVariant { args: xs, .. }
            | IrExprKind::Builtin { args: xs, .. } => {
                let mut state = state;
                for x in xs {
                    state = self.effects(x, state, reachable);
                }
                state
            }
            IrExprKind::GetField { recv, .. } => self.effects(recv, state, reachable),
            IrExprKind::Index { recv, index } => {
                let state = self.effects(recv, state, reachable);
                self.effects(index, state, reachable)
            }
            _ => state,
        }
    }

    fn effects_place(&mut self, p: &Place, state: Abs, reachable: bool) -> Abs {
        match p {
            Place::Var(_) => state,
            Place::Index { base, index, .. } => {
                let state = self.effects_place(base, state, reachable);
                self.effects(index, state, reachable)
            }
            Place::Field { base, .. } => self.effects_place(base, state, reachable),
        }
    }

    fn take_slot(&mut self) -> WalkCall {
        let oblig = self.oblig.as_mut().unwrap();
        if oblig.idx >= oblig.slots.len() {
            panic!(
                "internal error: walked more calls than sites in {}",
                self.module_name
            );
        }
        let slot = oblig.slots[oblig.idx].clone();
        oblig.idx += 1;
        slot
    }

    fn on_call(&mut self, name: &str, args: &[IrExpr], state: Abs, reachable: bool) -> Abs {
        if self.oblig.is_none() {
            return apply_inout(name, args, state, &self.root, self.inout_params);
        }
        let id = func_id(self.module_name, name);
        let WalkCall::Recorded(site) = self.take_slot() else {
            panic!(
                "internal error: call {name} has no pre-hoist site in {}",
                self.module_name
            );
        };
        if site.kind != CallKind::Direct || site.callee != id {
            panic!(
                "internal error: call {}.{} walked as {} at {}:{}",
                id.module, id.name, site.callee.name, site.line, site.col
            );
        }
        self.finish_recorded(site, args, state, reachable)
    }

    fn on_value_call(
        &mut self,
        _callee: &IrExpr,
        args: &[IrExpr],
        state: Abs,
        reachable: bool,
    ) -> Abs {
        if self.oblig.is_none() {
            return state;
        }
        match self.take_slot() {
            WalkCall::Indirect(_) => state,
            WalkCall::Recorded(site) if site.kind == CallKind::ResolvedValue => {
                self.finish_recorded(site, args, state, reachable)
            }
            WalkCall::Recorded(site) => panic!(
                "internal error: value call walked as direct {} at {}:{}",
                site.callee.name, site.line, site.col
            ),
        }
    }

    fn finish_recorded(
        &mut self,
        site: CallSite,
        args: &[IrExpr],
        state: Abs,
        reachable: bool,
    ) -> Abs {
        let callee_name = if site.callee.module == self.module_name {
            site.callee.name.clone()
        } else {
            format!("{}.{}", site.callee.module, site.callee.name)
        };
        if self.skip_obligations {
            return apply_inout(&callee_name, args, state, &self.root, self.inout_params);
        }
        let in_scc = self.oblig.as_ref().unwrap().scc.contains(&site.callee);
        let sites_ok = !reachable
            || !in_scc
            || call_decreases(
                &self.root,
                state,
                &site.callee,
                args,
                self.oblig.as_ref().unwrap().sites,
                self.modules,
                &self.copies,
            );
        if !sites_ok {
            self.oblig.as_mut().unwrap().errors.push(TypeError {
                line: site.line,
                col: site.col,
                msg: DECREASE_FAIL.to_string(),
            });
        }
        apply_inout(&callee_name, args, state, &self.root, self.inout_params)
    }

    fn mut_effect(&self, builtin: Builtin, recv: &Place, state: Abs) -> Abs {
        if place_root_name(recv) != self.root.local {
            return state;
        }
        match (self.root.kind, builtin) {
            (RootKind::Length, Builtin::ListPop | Builtin::ListRemoveAt) => apply_delta(state, -1),
            (RootKind::Length, Builtin::ListAppend | Builtin::ListInsert) => apply_delta(state, 1),
            (RootKind::Length, Builtin::ListSwap) => state,
            // Absent keys do not trap and do not shrink.
            (RootKind::Size, Builtin::SetRemove | Builtin::MapDelete) => Abs::Top,
            _ => Abs::Top,
        }
    }
}

fn apply_inout(
    name: &str,
    args: &[IrExpr],
    mut state: Abs,
    root: &MeasureRoot,
    inout_params: &HashMap<String, Vec<bool>>,
) -> Abs {
    if let Some(flags) = inout_params.get(name) {
        for (a, inout) in args.iter().zip(flags) {
            if *inout && expr_local(a) == Some(root.local.as_str()) {
                state = Abs::Top;
            }
        }
    }
    state
}

fn call_decreases(
    caller: &MeasureRoot,
    state: Abs,
    callee: &FuncId,
    args: &[IrExpr],
    sites: &BTreeMap<FuncId, FuncSites>,
    modules: &[IrModule],
    copies: &HashMap<String, Abs>,
) -> bool {
    let Some(measure) = sites.get(callee).and_then(|s| s.decreases.as_ref()) else {
        return false;
    };
    let callee_root = measure_root(measure);
    let func = find_func(modules, callee);
    let Some(idx) = func.params.iter().position(|p| p.name == callee_root.local) else {
        return false;
    };
    let Some(arg) = args.get(idx) else {
        return false;
    };
    matches!(
        eval_arg(caller, state, arg, copies),
        Abs::Rel { delta } if delta < 0
    )
}

fn eval_arg(caller: &MeasureRoot, state: Abs, arg: &IrExpr, copies: &HashMap<String, Abs>) -> Abs {
    if let IrExprKind::Local(n) = &arg.kind {
        if n == &caller.local {
            return state;
        }
        if let Some(saved) = copies.get(n) {
            return *saved;
        }
    }
    let Abs::Rel { .. } = state else {
        return Abs::Top;
    };
    if caller.kind == RootKind::Int {
        if let Some(change) = int_delta(&caller.local, arg) {
            return apply_delta(state, change);
        }
    }
    Abs::Top
}

fn writes_stmts(
    stmts: &[IrStmt],
    root: &MeasureRoot,
    inout_params: &HashMap<String, Vec<bool>>,
) -> bool {
    stmts.iter().any(|s| writes_stmt(s, root, inout_params))
}

fn writes_stmt(s: &IrStmt, root: &MeasureRoot, inout_params: &HashMap<String, Vec<bool>>) -> bool {
    match s {
        IrStmt::Assign { target, value, .. } => {
            place_root_name(target) == root.local || writes_expr(value, root, inout_params)
        }
        IrStmt::TupleAssign { targets, value, .. } => {
            targets.iter().any(|t| t == &root.local) || writes_expr(value, root, inout_params)
        }
        IrStmt::Expr(e) | IrStmt::Return(Some(e)) => writes_expr(e, root, inout_params),
        IrStmt::Assert { cond, .. } => writes_expr(cond, root, inout_params),
        IrStmt::If { arms, else_block } => {
            arms.iter().any(|(c, b)| {
                writes_expr(c, root, inout_params) || writes_stmts(b, root, inout_params)
            }) || else_block
                .as_ref()
                .is_some_and(|b| writes_stmts(b, root, inout_params))
        }
        IrStmt::While { cond, body } => {
            writes_expr(cond, root, inout_params) || writes_stmts(body, root, inout_params)
        }
        IrStmt::ForRange { from, to, body, .. } => {
            writes_expr(from, root, inout_params)
                || writes_expr(to, root, inout_params)
                || writes_stmts(body, root, inout_params)
        }
        IrStmt::ForIn { iter, body, .. } => {
            writes_expr(iter, root, inout_params) || writes_stmts(body, root, inout_params)
        }
        IrStmt::Match { scrutinee, arms } => {
            writes_expr(scrutinee, root, inout_params)
                || arms
                    .iter()
                    .any(|a| writes_stmts(&a.body, root, inout_params))
        }
        IrStmt::ExpectTrap { body, .. } => writes_stmts(body, root, inout_params),
        _ => false,
    }
}

fn writes_expr(e: &IrExpr, root: &MeasureRoot, inout_params: &HashMap<String, Vec<bool>>) -> bool {
    match &e.kind {
        IrExprKind::CallFunc { name, args } => {
            args.iter().any(|a| writes_expr(a, root, inout_params))
                || inout_params.get(name).is_some_and(|flags| {
                    args.iter()
                        .zip(flags)
                        .any(|(a, inout)| *inout && expr_local(a) == Some(root.local.as_str()))
                })
        }
        IrExprKind::CallValue { callee, args } => {
            writes_expr(callee, root, inout_params)
                || args.iter().any(|a| writes_expr(a, root, inout_params))
        }
        IrExprKind::MutBuiltin { recv, args, .. } => {
            place_root_name(recv) == root.local
                || args.iter().any(|a| writes_expr(a, root, inout_params))
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            xs.iter().any(|x| writes_expr(x, root, inout_params))
        }
        IrExprKind::GetField { recv, .. } => writes_expr(recv, root, inout_params),
        IrExprKind::Index { recv, index } => {
            writes_expr(recv, root, inout_params) || writes_expr(index, root, inout_params)
        }
        IrExprKind::Unary { operand, .. } => writes_expr(operand, root, inout_params),
        IrExprKind::Binary { lhs, rhs, .. } => {
            writes_expr(lhs, root, inout_params) || writes_expr(rhs, root, inout_params)
        }
        _ => false,
    }
}

// ---- structural subterms ---------------------------------------------------

#[derive(Clone)]
enum Class {
    Param(Ty),
    Strict(Ty),
    Container(ContainerOf),
}

#[derive(Clone)]
struct ContainerOf {
    value: Option<Ty>,
    key: Option<Ty>,
}

fn as_enum_container(ty: &Ty) -> Option<ContainerOf> {
    match ty {
        Ty::List(e) | Ty::Set(e) if matches!(e.as_ref(), Ty::Enum(_)) => Some(ContainerOf {
            value: Some((**e).clone()),
            key: None,
        }),
        Ty::Map(k, v) => {
            let key = if matches!(k.as_ref(), Ty::Enum(_)) {
                Some((**k).clone())
            } else {
                None
            };
            let value = if matches!(v.as_ref(), Ty::Enum(_)) {
                Some((**v).clone())
            } else {
                None
            };
            if key.is_none() && value.is_none() {
                None
            } else {
                Some(ContainerOf { value, key })
            }
        }
        _ => None,
    }
}

fn container_of_elem(projected: &Ty, elem: &Ty) -> Option<ContainerOf> {
    match projected {
        Ty::List(e) | Ty::Set(e) if e.as_ref() == elem => Some(ContainerOf {
            value: Some(elem.clone()),
            key: None,
        }),
        Ty::Map(k, v) => {
            let key = if k.as_ref() == elem {
                Some(elem.clone())
            } else {
                None
            };
            let value = if v.as_ref() == elem {
                Some(elem.clone())
            } else {
                None
            };
            if key.is_none() && value.is_none() {
                None
            } else {
                Some(ContainerOf { value, key })
            }
        }
        _ => None,
    }
}

fn project(src: &Class, projected: &Ty) -> Option<Class> {
    let src_ty = match src {
        Class::Param(t) | Class::Strict(t) => t,
        Class::Container(_) => return None,
    };
    if let Ty::Enum(_) = src_ty {
        if projected == src_ty {
            return Some(Class::Strict(projected.clone()));
        }
        if let Some(c) = container_of_elem(projected, src_ty) {
            return Some(Class::Container(c));
        }
        return None;
    }
    if matches!(src, Class::Param(_)) {
        if let Ty::Enum(_) = projected {
            return Some(Class::Strict(projected.clone()));
        }
        if let Some(c) = as_enum_container(projected) {
            return Some(Class::Container(c));
        }
    }
    None
}

fn param_can_descend(ty: &Ty, modules: &[IrModule], seen: &mut HashSet<String>) -> bool {
    match ty {
        Ty::Enum(_) => true,
        Ty::List(e) | Ty::Set(e) => matches!(e.as_ref(), Ty::Enum(_)),
        Ty::Map(k, v) => matches!(k.as_ref(), Ty::Enum(_)) || matches!(v.as_ref(), Ty::Enum(_)),
        Ty::Tuple(ts) => ts.iter().any(|t| param_can_descend(t, modules, seen)),
        Ty::Record(n) => {
            if !seen.insert(n.clone()) {
                return false;
            }
            sudoc_ir::find_record(modules, n).is_some_and(|r| {
                r.fields
                    .iter()
                    .any(|f| param_can_descend(&f.ty, modules, seen))
            })
        }
        _ => false,
    }
}

fn candidates(func: &IrFunc, modules: &[IrModule]) -> Vec<usize> {
    func.params
        .iter()
        .enumerate()
        .filter(|(_, p)| p.never_written && param_can_descend(&p.ty, modules, &mut HashSet::new()))
        .map(|(i, _)| i)
        .collect()
}

fn scc_structural(
    modules: &[IrModule],
    comp: &[FuncId],
    sites: &BTreeMap<FuncId, FuncSites>,
) -> bool {
    let unann: Vec<&FuncId> = comp
        .iter()
        .filter(|id| sites.get(id).and_then(|s| s.decreases.as_ref()).is_none())
        .collect();
    if unann.is_empty() {
        return true;
    }
    let cands: Vec<Vec<usize>> = unann
        .iter()
        .map(|id| candidates(find_func(modules, id), modules))
        .collect();
    let mut product = 1u64;
    for c in &cands {
        if c.is_empty() {
            return false;
        }
        product = product.saturating_mul(c.len() as u64);
        if product > STRUCT_FUEL {
            return false;
        }
    }
    let comp_set: HashSet<FuncId> = comp.iter().cloned().collect();
    let mut choice = vec![0usize; unann.len()];
    loop {
        let mut map = HashMap::new();
        for (i, id) in unann.iter().enumerate() {
            map.insert((*id).clone(), cands[i][choice[i]]);
        }
        if unann
            .iter()
            .all(|id| calls_descend(modules, id, map[id], &map, &comp_set, &sites[id]))
        {
            return true;
        }
        if !bump_choice(&mut choice, &cands) {
            return false;
        }
    }
}

fn bump_choice(choice: &mut [usize], cands: &[Vec<usize>]) -> bool {
    for i in (0..choice.len()).rev() {
        if choice[i] + 1 < cands[i].len() {
            choice[i] += 1;
            for c in choice.iter_mut().skip(i + 1) {
                *c = 0;
            }
            return true;
        }
    }
    false
}

fn calls_descend(
    modules: &[IrModule],
    id: &FuncId,
    param_index: usize,
    choice: &HashMap<FuncId, usize>,
    scc: &HashSet<FuncId>,
    sites: &FuncSites,
) -> bool {
    let module = module_of(modules, &id.module);
    let func = find_func(modules, id);
    let written = written_locals(func, module, modules);
    let param = &func.params[param_index];
    let mut env = HashMap::new();
    env.insert(param.name.clone(), root_class(&param.ty));
    let cx = StructCx {
        modules,
        module_name: &module.name,
        written: &written,
        choice,
        scc,
        slots: &sites.slots,
        slot_i: Cell::new(0),
    };
    let ok = struct_stmts(&func.body, &mut env, &cx);
    if ok && cx.slot_i.get() != cx.slots.len() {
        panic!(
            "internal error: call sites ({}) != walked calls ({}) in {}.{}",
            cx.slots.len(),
            cx.slot_i.get(),
            id.module,
            id.name
        );
    }
    ok
}

fn root_class(ty: &Ty) -> Class {
    if let Some(c) = as_enum_container(ty) {
        Class::Container(c)
    } else {
        Class::Param(ty.clone())
    }
}

struct StructCx<'a> {
    modules: &'a [IrModule],
    module_name: &'a str,
    written: &'a HashSet<String>,
    choice: &'a HashMap<FuncId, usize>,
    scc: &'a HashSet<FuncId>,
    slots: &'a [WalkCall],
    slot_i: Cell<usize>,
}

impl StructCx<'_> {
    fn take_slot(&self) -> &WalkCall {
        let i = self.slot_i.get();
        let Some(slot) = self.slots.get(i) else {
            panic!(
                "internal error: walked more calls than sites in {}",
                self.module_name
            );
        };
        self.slot_i.set(i + 1);
        slot
    }
}

fn struct_stmts(stmts: &[IrStmt], env: &mut HashMap<String, Class>, cx: &StructCx<'_>) -> bool {
    for s in stmts {
        if !struct_stmt(s, env, cx) {
            return false;
        }
    }
    true
}

fn struct_stmt(s: &IrStmt, env: &mut HashMap<String, Class>, cx: &StructCx<'_>) -> bool {
    match s {
        IrStmt::Assign {
            target,
            value,
            declares,
        } => {
            if !struct_expr(value, env, cx) || !struct_place(target, env, cx) {
                return false;
            }
            if let Place::Var(n) = target {
                if *declares && !cx.written.contains(n) {
                    if let Some(cls) = class_of_expr(value, env) {
                        env.insert(n.clone(), cls);
                    }
                }
            }
            true
        }
        IrStmt::TupleAssign {
            targets,
            declares,
            value,
        } => {
            if !struct_expr(value, env, cx) {
                return false;
            }
            if let IrExprKind::Local(src) = &value.kind {
                if let (Some(cls), Ty::Tuple(ts)) = (env.get(src).cloned(), &value.ty) {
                    for ((name, decl), ty) in targets.iter().zip(declares).zip(ts) {
                        if *decl && !cx.written.contains(name) {
                            if let Some(p) = project(&cls, ty) {
                                env.insert(name.clone(), p);
                            }
                        }
                    }
                }
            }
            true
        }
        IrStmt::Expr(e) | IrStmt::Return(Some(e)) | IrStmt::Assert { cond: e, .. } => {
            struct_expr(e, env, cx)
        }
        IrStmt::If { arms, else_block } => {
            let saved = env.clone();
            for (c, b) in arms {
                if !struct_expr(c, &saved, cx) {
                    return false;
                }
                let mut e = saved.clone();
                if !struct_stmts(b, &mut e, cx) {
                    return false;
                }
            }
            if let Some(b) = else_block {
                let mut e = saved.clone();
                if !struct_stmts(b, &mut e, cx) {
                    return false;
                }
            }
            true
        }
        IrStmt::While { cond, body } => {
            if !struct_expr(cond, env, cx) {
                return false;
            }
            let mut e = env.clone();
            struct_stmts(body, &mut e, cx)
        }
        IrStmt::ForRange { from, to, body, .. } => {
            if !struct_expr(from, env, cx) || !struct_expr(to, env, cx) {
                return false;
            }
            let mut e = env.clone();
            struct_stmts(body, &mut e, cx)
        }
        IrStmt::ForIn { vars, iter, body } => {
            if !struct_expr(iter, env, cx) {
                return false;
            }
            let mut e = env.clone();
            if let Some(cls) = class_of_expr(iter, env) {
                bind_for_in(vars, &iter.ty, &cls, cx.written, &mut e);
            }
            struct_stmts(body, &mut e, cx)
        }
        IrStmt::Match { scrutinee, arms } => {
            if !struct_expr(scrutinee, env, cx) {
                return false;
            }
            let saved = env.clone();
            let scrut_cls = class_of_expr(scrutinee, &saved);
            for arm in arms {
                let mut e = saved.clone();
                if let (
                    Some(cls),
                    IrPattern::Variant {
                        enum_name,
                        variant,
                        binders,
                    },
                ) = (&scrut_cls, &arm.pattern)
                {
                    if let Some(fields) = variant_fields(cx.modules, enum_name, variant) {
                        for (binder, ty) in binders.iter().zip(fields) {
                            if cx.written.contains(binder) {
                                continue;
                            }
                            if let Some(p) = project(cls, &ty) {
                                e.insert(binder.clone(), p);
                            }
                        }
                    }
                }
                if !struct_stmts(&arm.body, &mut e, cx) {
                    return false;
                }
            }
            true
        }
        IrStmt::ExpectTrap { body, .. } => {
            let mut e = env.clone();
            struct_stmts(body, &mut e, cx)
        }
        IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => true,
    }
}

fn bind_for_in(
    vars: &[String],
    iter_ty: &Ty,
    cls: &Class,
    written: &HashSet<String>,
    env: &mut HashMap<String, Class>,
) {
    let Class::Container(c) = cls else {
        return;
    };
    match iter_ty {
        Ty::List(_) | Ty::Set(_) => {
            if let (Some(name), Some(elem)) = (vars.first(), &c.value) {
                if !written.contains(name) {
                    env.insert(name.clone(), Class::Strict(elem.clone()));
                }
            }
        }
        Ty::Map(_, _) if vars.len() == 2 => {
            if let Some(k) = &c.key {
                if !written.contains(&vars[0]) {
                    env.insert(vars[0].clone(), Class::Strict(k.clone()));
                }
            }
            if let Some(v) = &c.value {
                if !written.contains(&vars[1]) {
                    env.insert(vars[1].clone(), Class::Strict(v.clone()));
                }
            }
        }
        _ => {}
    }
}

fn variant_fields(modules: &[IrModule], enum_name: &str, variant: &str) -> Option<Vec<Ty>> {
    let en = sudoc_ir::find_enum(modules, enum_name)?;
    let v = en.variants.iter().find(|v| v.name == variant)?;
    Some(v.fields.iter().map(|f| f.ty.clone()).collect())
}

fn struct_place(p: &Place, env: &HashMap<String, Class>, cx: &StructCx<'_>) -> bool {
    match p {
        Place::Var(_) => true,
        Place::Index { base, index, .. } => {
            struct_place(base, env, cx) && struct_expr(index, env, cx)
        }
        Place::Field { base, .. } => struct_place(base, env, cx),
    }
}

fn struct_expr(e: &IrExpr, env: &HashMap<String, Class>, cx: &StructCx<'_>) -> bool {
    match &e.kind {
        IrExprKind::CallFunc { name, args } => {
            if !args.iter().all(|a| struct_expr(a, env, cx)) {
                return false;
            }
            let id = func_id(cx.module_name, name);
            let WalkCall::Recorded(site) = cx.take_slot() else {
                panic!(
                    "internal error: call {name} has no pre-hoist site in {}",
                    cx.module_name
                );
            };
            if site.kind != CallKind::Direct || site.callee != id {
                panic!(
                    "internal error: call {}.{} walked as {} at {}:{}",
                    id.module, id.name, site.callee.name, site.line, site.col
                );
            }
            arg_descends(&site.callee, args, env, cx.choice, cx.scc)
        }
        IrExprKind::CallValue { callee, args } => {
            if !struct_expr(callee, env, cx) || !args.iter().all(|a| struct_expr(a, env, cx)) {
                return false;
            }
            match cx.take_slot() {
                WalkCall::Indirect(_) => true,
                WalkCall::Recorded(site) if site.kind == CallKind::ResolvedValue => {
                    arg_descends(&site.callee, args, env, cx.choice, cx.scc)
                }
                WalkCall::Recorded(site) => panic!(
                    "internal error: value call walked as direct {} at {}:{}",
                    site.callee.name, site.line, site.col
                ),
            }
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => xs.iter().all(|x| struct_expr(x, env, cx)),
        IrExprKind::MutBuiltin { recv, args, .. } => {
            args.iter().all(|a| struct_expr(a, env, cx)) && struct_place(recv, env, cx)
        }
        IrExprKind::GetField { recv, .. } => struct_expr(recv, env, cx),
        IrExprKind::Index { recv, index } => {
            struct_expr(recv, env, cx) && struct_expr(index, env, cx)
        }
        IrExprKind::Unary { operand, .. } => struct_expr(operand, env, cx),
        IrExprKind::Binary { lhs, rhs, .. } => {
            struct_expr(lhs, env, cx) && struct_expr(rhs, env, cx)
        }
        _ => true,
    }
}

fn arg_descends(
    callee: &FuncId,
    args: &[IrExpr],
    env: &HashMap<String, Class>,
    choice: &HashMap<FuncId, usize>,
    scc: &HashSet<FuncId>,
) -> bool {
    if !scc.contains(callee) {
        return true;
    }
    let Some(&idx) = choice.get(callee) else {
        return false;
    };
    let Some(arg) = args.get(idx) else {
        return false;
    };
    let IrExprKind::Local(n) = &arg.kind else {
        return false;
    };
    matches!(env.get(n), Some(Class::Strict(_)))
}

fn class_of_expr(expr: &IrExpr, env: &HashMap<String, Class>) -> Option<Class> {
    match &expr.kind {
        IrExprKind::Local(n) => env.get(n).cloned(),
        IrExprKind::GetField { recv, .. } => {
            let src = class_of_expr(recv, env)?;
            project(&src, &expr.ty)
        }
        IrExprKind::Index { recv, index } => {
            if !index_is_pure(index) {
                return None;
            }
            let src = class_of_expr(recv, env)?;
            let Class::Container(c) = src else {
                return None;
            };
            let elem = c.value?;
            if expr.ty == elem {
                Some(Class::Strict(elem))
            } else {
                None
            }
        }
        IrExprKind::MutBuiltin {
            builtin: Builtin::ListPop | Builtin::ListRemoveAt,
            recv,
            ..
        } => {
            let Place::Var(n) = recv else {
                return None;
            };
            let Class::Container(c) = env.get(n)?.clone() else {
                return None;
            };
            let elem = c.value?;
            if expr.ty == elem {
                Some(Class::Strict(elem))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn index_is_pure(e: &IrExpr) -> bool {
    match &e.kind {
        IrExprKind::Int(_)
        | IrExprKind::Float(_)
        | IrExprKind::Bool(_)
        | IrExprKind::Text(_)
        | IrExprKind::Local(_)
        | IrExprKind::Const(_) => true,
        IrExprKind::Unary { operand, .. } => index_is_pure(operand),
        IrExprKind::Binary { lhs, rhs, .. } => index_is_pure(lhs) && index_is_pure(rhs),
        IrExprKind::Builtin { args, .. } => args.iter().all(index_is_pure),
        _ => false,
    }
}
