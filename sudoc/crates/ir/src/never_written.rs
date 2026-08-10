//! Whole-program analysis: which function parameters are never written by
//! their callee body.
//!
//! Populates [`IrParam::never_written`](crate::IrParam::never_written). That
//! flag is a FACT about the callee body only — assignment, index/field
//! mutation, mutating-builtin receiver, or forwarding as an `inout` argument.
//! It says nothing about aliasing at call sites or whether the function's
//! address is taken as a `FuncRef` value.
//!
//! Backends that change calling conventions (Zig borrows) must combine this
//! flag with [`compute_address_taken`] / [`func_is_address_taken`]. Backends
//! that only skip an internal entry-copy (Python, JS) may use the flag alone,
//! but must still guard call sites where the same root local is passed both
//! `inout` and by-value (see backend Guard 2).

use std::collections::{HashMap, HashSet};

use crate::{IrExpr, IrExprKind, IrFunc, IrModule, IrParam, IrStmt, Place};

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
            written.insert((m.name.clone(), f.name.clone()), written_params(f, m, modules));
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
    collect_written(&f.body, m, all, &params, &mut out);
    out
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
pub fn func_is_address_taken(owning: &IrModule, f: &IrFunc, address_taken: &HashSet<String>) -> bool {
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
        IrExprKind::MutBuiltin {
            recv,
            args,
            ..
        } => {
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
        Place::Index {
            base,
            index,
            ..
        } => {
            collect_funcrefs_place(base, out);
            collect_funcrefs_expr(index, out);
        }
        Place::Field { base, .. } => {
            collect_funcrefs_place(base, out);
        }
    }
}

fn collect_written(
    stmts: &[IrStmt],
    m: &IrModule,
    all: &[IrModule],
    params: &HashSet<String>,
    out: &mut HashSet<String>,
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
                        if !declares && params.contains(n) {
                            out.insert(n.clone());
                        }
                    }
                    _ => {
                        let r = place_root(target);
                        if params.contains(r) {
                            out.insert(r.to_string());
                        }
                    }
                }
                written_expr(value, m, all, params, out);
            }
            IrStmt::TupleAssign {
                targets,
                declares,
                value,
            } => {
                for (t, d) in targets.iter().zip(declares) {
                    if !d && params.contains(t) {
                        out.insert(t.clone());
                    }
                }
                written_expr(value, m, all, params, out);
            }
            IrStmt::Expr(e) => written_expr(e, m, all, params, out),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    written_expr(c, m, all, params, out);
                    collect_written(b, m, all, params, out);
                }
                if let Some(b) = else_block {
                    collect_written(b, m, all, params, out);
                }
            }
            IrStmt::While { cond, body } => {
                written_expr(cond, m, all, params, out);
                collect_written(body, m, all, params, out);
            }
            IrStmt::ForRange {
                from, to, body, ..
            } => {
                written_expr(from, m, all, params, out);
                written_expr(to, m, all, params, out);
                collect_written(body, m, all, params, out);
            }
            IrStmt::ForIn { iter, body, .. } => {
                written_expr(iter, m, all, params, out);
                collect_written(body, m, all, params, out);
            }
            IrStmt::Match { scrutinee, arms } => {
                written_expr(scrutinee, m, all, params, out);
                for a in arms {
                    collect_written(&a.body, m, all, params, out);
                }
            }
            IrStmt::Return(Some(e)) => written_expr(e, m, all, params, out),
            IrStmt::Assert { cond, .. } => written_expr(cond, m, all, params, out),
            IrStmt::ExpectTrap { body, .. } => collect_written(body, m, all, params, out),
            _ => {}
        }
    }
}

fn written_expr(
    e: &IrExpr,
    m: &IrModule,
    all: &[IrModule],
    params: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    match &e.kind {
        IrExprKind::MutBuiltin { recv, args, .. } => {
            let r = place_root(recv);
            if params.contains(r) {
                out.insert(r.to_string());
            }
            for a in args {
                written_expr(a, m, all, params, out);
            }
        }
        IrExprKind::CallFunc { name, args } => {
            if let Some(cf) = resolve_func_in(m, all, name) {
                for (arg, p) in args.iter().zip(&cf.params) {
                    if p.inout {
                        if let Some(r) = expr_root_var(arg) {
                            if params.contains(r) {
                                out.insert(r.to_string());
                            }
                        }
                    }
                }
            }
            for a in args {
                written_expr(a, m, all, params, out);
            }
        }
        IrExprKind::CallValue { callee, args } => {
            written_expr(callee, m, all, params, out);
            for a in args {
                written_expr(a, m, all, params, out);
            }
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            for x in xs {
                written_expr(x, m, all, params, out);
            }
        }
        IrExprKind::GetField { recv, .. } => written_expr(recv, m, all, params, out),
        IrExprKind::Index { recv, index } => {
            written_expr(recv, m, all, params, out);
            written_expr(index, m, all, params, out);
        }
        IrExprKind::Unary { operand, .. } => written_expr(operand, m, all, params, out),
        IrExprKind::Binary { lhs, rhs, .. } => {
            written_expr(lhs, m, all, params, out);
            written_expr(rhs, m, all, params, out);
        }
        _ => {}
    }
}
