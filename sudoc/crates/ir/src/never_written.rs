//! Whole-program analysis: which names are never written by a body.
//!
//! Populates [`IrParam::never_written`](crate::IrParam::never_written). That
//! flag is a FACT about the callee body only — assignment, index/field
//! mutation, mutating-builtin receiver, or forwarding as an `inout` argument.
//! It says nothing about aliasing at call sites or whether the function's
//! address is taken as a `FuncRef` value.
//!
//! [`written_locals`] / [`written_in_stmts`] expose the same write-set for
//! every local (not just parameters). py/js use it to skip `dup` on a
//! tuple destructure whose bindings are only read.
//!
//! [`field_written_records`] reads the same write sites by type: the record
//! types whose fields some body writes in place. js shares the values of
//! every other record type instead of copying them.
//!
//! Backends that change calling conventions (Zig borrows) must combine this
//! flag with [`compute_address_taken`] / [`func_is_address_taken`]. Backends
//! that only skip an internal entry-copy (Python, JS) may use the flag alone,
//! but must still guard call sites where the same root local is passed both
//! `inout` and by-value (see backend Guard 2).

use std::collections::{HashMap, HashSet};

use crate::{IrExpr, IrExprKind, IrFunc, IrModule, IrParam, IrStmt, Place, Ty};

/// Set `never_written` on every non-inout parameter of every function
/// (`IrFunc.params`) across the WHOLE program, so cross-module callees
/// resolve against their owning module (spec: multi-module programs).
/// `inout` params are left `false` (unused by every consumer, which already
/// gates on `p.inout` first). Call once, after monomorphization — see
/// `sudoc_types::check` / `check_program_with`.
///
/// IMPORTANT — layering: this sets only the FACT "the callee's body never
/// writes this param". It does NOT know or care whether the function's
/// address is taken (used as a `FuncRef` value). Zig's calling convention
/// needs that extra restriction (a function whose address is taken must
/// keep one uniform signature across all call sites) — see
/// `compute_address_taken`/`func_is_address_taken` above, which backend_zig
/// combines with this flag itself. backend_py/backend_js do NOT change any
/// function signature (they only skip an internal entry-copy statement), so
/// they may — and, for the comparator-passed-as-a-value workload this exists
/// for, MUST — use the flag alone, without any address-taken restriction.
pub fn annotate(modules: &mut [IrModule]) {
    // Pass 1: read-only over the whole program (needed so written_params can
    // resolve cross-module callees against `modules`), collect one written-
    // param-name-set per (module name, func name).
    let mut written: HashMap<(String, String), HashSet<String>> = HashMap::new();
    for m in modules.iter() {
        for f in &m.funcs {
            written.insert(
                (m.name.clone(), f.name.clone()),
                written_params(f, m, modules),
            );
        }
    }
    // Pass 2: mutate. No more borrows of `modules` as a whole are alive here.
    for m in modules.iter_mut() {
        let mname = m.name.clone();
        for f in &mut m.funcs {
            let w = written.get(&(mname.clone(), f.name.clone()));
            for p in &mut f.params {
                p.never_written = if p.inout {
                    false
                } else {
                    w.map(|s| !s.contains(&p.name)).unwrap_or(true)
                };
            }
        }
    }
}

/// Non-inout parameters the body writes to (rebind, field/index assignment,
/// mutating method receiver, or passed as an inout argument).
fn written_params(f: &IrFunc, m: &IrModule, all: &[IrModule]) -> HashSet<String> {
    let params: HashSet<String> = f
        .params
        .iter()
        .filter(|p| !p.inout)
        .map(|p| p.name.clone())
        .collect();
    let mut out = HashSet::new();
    collect_written(&f.body, m, all, Some(&params), &mut out);
    out
}

/// Locals the body writes (rebind, field/index mutation, mutating-builtin
/// receiver, or passed as `inout`). Used to skip `dup` on a destructure
/// whose bindings are only read.
pub fn written_locals(f: &IrFunc, m: &IrModule, all: &[IrModule]) -> HashSet<String> {
    written_in_stmts(&f.body, m, all)
}

/// Same analysis over an arbitrary statement list (test bodies).
pub fn written_in_stmts(stmts: &[IrStmt], m: &IrModule, all: &[IrModule]) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_written(stmts, m, all, None, &mut out);
    out
}

fn watched(filter: Option<&HashSet<String>>, name: &str) -> bool {
    match filter {
        None => true,
        Some(s) => s.contains(name),
    }
}

fn place_root(p: &Place) -> &str {
    match p {
        Place::Var(n) => n,
        Place::Field { base, .. } | Place::Index { base, .. } => place_root(base),
    }
}

/// Root local variable of an expression that is a field/index chain from a
/// `Local`, if any. Used by backends for call-site aliasing guards.
pub fn expr_root_var(e: &IrExpr) -> Option<&str> {
    match &e.kind {
        IrExprKind::Local(n) => Some(n),
        IrExprKind::GetField { recv, .. } => expr_root_var(recv),
        IrExprKind::Index { recv, .. } => expr_root_var(recv),
        _ => None,
    }
}

/// Guard 2: the set of local roots passed as `inout` arguments in a
/// single call (keyed by [`expr_root_var`]). Backends that skip the
/// entry-copy for `never_written` by-value params (Python, JS — see
/// module doc) must re-dup any by-value argument whose root local is
/// ALSO passed `inout` in the same call, or the by-value copy would
/// alias the inout write.
pub fn inout_roots<'a>(args: &'a [IrExpr], params: &[IrParam]) -> HashSet<&'a str> {
    args.iter()
        .zip(params)
        .filter(|(_, p)| p.inout)
        .filter_map(|(a, _)| expr_root_var(a))
        .collect()
}

/// Resolve a callee's signature across the whole program: a bare name
/// looks in `m` (the current module); a `module.func` qualified name
/// looks up the named module in `all`.
fn resolve_func_in<'a>(m: &'a IrModule, all: &'a [IrModule], name: &str) -> Option<&'a IrFunc> {
    match name.split_once('.') {
        Some((modname, fname)) => all.iter().find(|mm| mm.name == modname)?.func(fname),
        None => m.func(name),
    }
}

/// Conservative address-taken test: match either the bare function name or
/// the `module.func` spelling. Over-approximation only loses the borrow
/// optimization — never correctness.
pub fn func_is_address_taken(
    owning: &IrModule,
    f: &IrFunc,
    address_taken: &HashSet<String>,
) -> bool {
    address_taken.contains(&f.name)
        || address_taken.contains(&format!("{}.{}", owning.name, f.name))
}

/// Whole-program set of function names that appear as `FuncRef` values.
pub fn compute_address_taken(modules: &[IrModule]) -> HashSet<String> {
    let mut out = HashSet::new();
    for m in modules {
        for f in &m.funcs {
            collect_funcrefs_stmts(&f.body, &mut out);
        }
        for t in &m.tests {
            collect_funcrefs_stmts(&t.body, &mut out);
        }
        for c in &m.consts {
            collect_funcrefs_expr(&c.value, &mut out);
        }
    }
    out
}

fn collect_funcrefs_stmts(stmts: &[IrStmt], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                collect_funcrefs_place(target, out);
                collect_funcrefs_expr(value, out);
            }
            IrStmt::TupleAssign { value, .. } => collect_funcrefs_expr(value, out),
            IrStmt::Expr(e) => collect_funcrefs_expr(e, out),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    collect_funcrefs_expr(c, out);
                    collect_funcrefs_stmts(b, out);
                }
                if let Some(b) = else_block {
                    collect_funcrefs_stmts(b, out);
                }
            }
            IrStmt::While { cond, body } => {
                collect_funcrefs_expr(cond, out);
                collect_funcrefs_stmts(body, out);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                collect_funcrefs_expr(from, out);
                collect_funcrefs_expr(to, out);
                collect_funcrefs_stmts(body, out);
            }
            IrStmt::ForIn { iter, body, .. } => {
                collect_funcrefs_expr(iter, out);
                collect_funcrefs_stmts(body, out);
            }
            IrStmt::Match { scrutinee, arms } => {
                collect_funcrefs_expr(scrutinee, out);
                for a in arms {
                    collect_funcrefs_stmts(&a.body, out);
                }
            }
            IrStmt::Return(Some(e)) => collect_funcrefs_expr(e, out),
            IrStmt::Assert { cond, .. } => collect_funcrefs_expr(cond, out),
            IrStmt::ExpectTrap { body, .. } => collect_funcrefs_stmts(body, out),
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
}

fn collect_funcrefs_expr(e: &IrExpr, out: &mut HashSet<String>) {
    match &e.kind {
        IrExprKind::FuncRef(n) => {
            out.insert(n.clone());
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            xs.iter().for_each(|x| collect_funcrefs_expr(x, out))
        }
        IrExprKind::CallValue { callee, args } => {
            collect_funcrefs_expr(callee, out);
            args.iter().for_each(|x| collect_funcrefs_expr(x, out));
        }
        IrExprKind::MutBuiltin { recv, args, .. } => {
            collect_funcrefs_place(recv, out);
            args.iter().for_each(|x| collect_funcrefs_expr(x, out));
        }
        IrExprKind::GetField { recv, .. } => collect_funcrefs_expr(recv, out),
        IrExprKind::Index { recv, index } => {
            collect_funcrefs_expr(recv, out);
            collect_funcrefs_expr(index, out);
        }
        IrExprKind::Unary { operand, .. } => collect_funcrefs_expr(operand, out),
        IrExprKind::Binary { lhs, rhs, .. } => {
            collect_funcrefs_expr(lhs, out);
            collect_funcrefs_expr(rhs, out);
        }
        _ => {}
    }
}

fn collect_funcrefs_place(p: &Place, out: &mut HashSet<String>) {
    match p {
        Place::Var(_) => {}
        Place::Index { base, index, .. } => {
            collect_funcrefs_place(base, out);
            collect_funcrefs_expr(index, out);
        }
        Place::Field { base, .. } => {
            collect_funcrefs_place(base, out);
        }
    }
}

/// One write site in a body: a rebinding of a local, an assigned or mutated
/// place, or an argument passed `inout`.
enum Write<'a> {
    Bind(&'a str),
    Place(&'a Place),
    Inout(&'a IrExpr),
}

fn collect_written(
    stmts: &[IrStmt],
    m: &IrModule,
    all: &[IrModule],
    filter: Option<&HashSet<String>>,
    out: &mut HashSet<String>,
) {
    visit_writes(stmts, m, all, &mut |w| {
        let root = match w {
            Write::Bind(n) => Some(n),
            Write::Place(p) => Some(place_root(p)),
            Write::Inout(e) => expr_root_var(e),
        };
        if let Some(r) = root.filter(|r| watched(filter, r)) {
            out.insert(r.to_string());
        }
    });
}

/// Record types whose fields some body in the program writes in place: a
/// field on an assigned or mutated place (`r.f = v`, `r.f.append(x)`,
/// `xs[i].f[j] = v`) or on a path passed `inout`. Values of every other
/// record type are only ever built whole and read, so sharing one is
/// unobservable. Test bodies count, so a build with and without tests agrees.
pub fn field_written_records(all: &[IrModule]) -> HashSet<String> {
    let mut out = HashSet::new();
    for m in all {
        let bodies = m
            .funcs
            .iter()
            .map(|f| &f.body)
            .chain(m.tests.iter().map(|t| &t.body));
        for body in bodies {
            visit_writes(body, m, all, &mut |w| match w {
                Write::Bind(_) => {}
                Write::Place(p) => place_field_records(p, &mut out),
                Write::Inout(e) => expr_field_records(e, &mut out),
            });
        }
    }
    out
}

fn place_field_records(p: &Place, out: &mut HashSet<String>) {
    match p {
        Place::Var(_) => {}
        Place::Index { base, .. } => place_field_records(base, out),
        Place::Field { base, base_ty, .. } => {
            if let Ty::Record(n) = base_ty {
                out.insert(n.clone());
            }
            place_field_records(base, out);
        }
    }
}

fn expr_field_records(e: &IrExpr, out: &mut HashSet<String>) {
    // Inout arguments are a plain variable or a record-field path.
    if let IrExprKind::GetField { recv, .. } = &e.kind {
        if let Ty::Record(n) = &recv.ty {
            out.insert(n.clone());
        }
        expr_field_records(recv, out);
    }
}

fn visit_writes<'a>(
    stmts: &'a [IrStmt],
    m: &'a IrModule,
    all: &'a [IrModule],
    f: &mut dyn FnMut(Write<'a>),
) {
    for s in stmts {
        match s {
            IrStmt::Assign {
                target,
                value,
                declares,
            } => {
                match target {
                    Place::Var(n) => {
                        if !declares {
                            f(Write::Bind(n));
                        }
                    }
                    _ => f(Write::Place(target)),
                }
                visit_writes_expr(value, m, all, f);
            }
            IrStmt::TupleAssign {
                targets,
                declares,
                value,
            } => {
                for (t, d) in targets.iter().zip(declares) {
                    if !d {
                        f(Write::Bind(t));
                    }
                }
                visit_writes_expr(value, m, all, f);
            }
            IrStmt::Expr(e) => visit_writes_expr(e, m, all, f),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    visit_writes_expr(c, m, all, f);
                    visit_writes(b, m, all, f);
                }
                if let Some(b) = else_block {
                    visit_writes(b, m, all, f);
                }
            }
            IrStmt::While { cond, body } => {
                visit_writes_expr(cond, m, all, f);
                visit_writes(body, m, all, f);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                visit_writes_expr(from, m, all, f);
                visit_writes_expr(to, m, all, f);
                visit_writes(body, m, all, f);
            }
            IrStmt::ForIn { iter, body, .. } => {
                visit_writes_expr(iter, m, all, f);
                visit_writes(body, m, all, f);
            }
            IrStmt::Match { scrutinee, arms } => {
                visit_writes_expr(scrutinee, m, all, f);
                for a in arms {
                    visit_writes(&a.body, m, all, f);
                }
            }
            IrStmt::Return(Some(e)) => visit_writes_expr(e, m, all, f),
            IrStmt::Assert { cond, .. } => visit_writes_expr(cond, m, all, f),
            IrStmt::ExpectTrap { body, .. } => visit_writes(body, m, all, f),
            _ => {}
        }
    }
}

fn visit_writes_expr<'a>(
    e: &'a IrExpr,
    m: &'a IrModule,
    all: &'a [IrModule],
    f: &mut dyn FnMut(Write<'a>),
) {
    match &e.kind {
        IrExprKind::MutBuiltin { recv, args, .. } => {
            f(Write::Place(recv));
            for a in args {
                visit_writes_expr(a, m, all, f);
            }
        }
        IrExprKind::CallFunc { name, args } => {
            if let Some(cf) = resolve_func_in(m, all, name) {
                for (arg, p) in args.iter().zip(&cf.params) {
                    if p.inout {
                        f(Write::Inout(arg));
                    }
                }
            }
            for a in args {
                visit_writes_expr(a, m, all, f);
            }
        }
        IrExprKind::CallValue { callee, args } => {
            visit_writes_expr(callee, m, all, f);
            for a in args {
                visit_writes_expr(a, m, all, f);
            }
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            for x in xs {
                visit_writes_expr(x, m, all, f);
            }
        }
        IrExprKind::GetField { recv, .. } => visit_writes_expr(recv, m, all, f),
        IrExprKind::Index { recv, index } => {
            visit_writes_expr(recv, m, all, f);
            visit_writes_expr(index, m, all, f);
        }
        IrExprKind::Unary { operand, .. } => visit_writes_expr(operand, m, all, f),
        IrExprKind::Binary { lhs, rhs, .. } => {
            visit_writes_expr(lhs, m, all, f);
            visit_writes_expr(rhs, m, all, f);
        }
        _ => {}
    }
}
