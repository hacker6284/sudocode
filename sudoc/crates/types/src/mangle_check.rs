//! Program-wide uniqueness check for mangled type symbols.
//!
//! `mangle_ty` embeds record/enum names bare inside composite tags, so two
//! structurally different types can produce the same mangled string (e.g.
//! `record List_3i64` vs `List<int>`, or `Map<a_b, c>` vs `Map<a, b_c>`).
//! Changing the mangling grammar would churn every backend golden and is
//! out of scope; instead, after monomorphization finishes, walk every type
//! reachable in the checked program and reject any mangled symbol claimed
//! by two distinct `Ty` values. Same-`Ty` reuse is normal and allowed.
//!
//! Complexity is O(number of type nodes visited): one `BTreeMap` lookup per
//! observed type against the single existing entry for that mangled key.

use std::collections::BTreeMap;

use sudoc_ir::mangle::mangle_ty;
use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrStmt, Place, Ty};

use crate::{error, TypeError};

/// Surface form for diagnostics. Bare record/enum names alone are ambiguous
/// next to a composite that mangles to the same spelling, so prefix them
/// with the keyword (matching `func_check`'s `"record {name}"` phrasing).
fn describe(ty: &Ty) -> String {
    match ty {
        Ty::Record(n) => format!("record {n}"),
        Ty::Enum(n) => format!("enum {n}"),
        other => other.to_string(),
    }
}

fn is_scalar(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::Float | Ty::Bool)
}

/// Map from mangled symbol → the first `Ty` that produced it. Ordered so
/// walk/error order is deterministic regardless of HashMap iteration.
struct CollisionMap {
    by_mangle: BTreeMap<String, Ty>,
}

impl CollisionMap {
    fn new() -> Self {
        Self { by_mangle: BTreeMap::new() }
    }

    /// Register `ty` and its component types. Returns an error if a different
    /// structural type already claimed this mangled string.
    fn observe(&mut self, ty: &Ty) -> Result<(), TypeError> {
        // Scalars cannot collide (length-prefixed leaf encodings). Empty
        // tuple is the checker's void sentinel and never materializes.
        // Infer never survives monomorphization; skip defensively.
        if is_scalar(ty) || matches!(ty, Ty::Tuple(ts) if ts.is_empty()) || matches!(ty, Ty::Infer(_))
        {
            return Ok(());
        }
        let mangled = mangle_ty(ty);
        match self.by_mangle.get(&mangled) {
            Some(existing) if existing != ty => {
                return error(
                    0,
                    0,
                    format!(
                        "mangled symbol '{mangled}' is shared by two distinct types: {} and {} — \
                         rename one of the conflicting records/enums",
                        describe(existing),
                        describe(ty)
                    ),
                );
            }
            Some(_) => {
                // Same Ty already collected (with its components) — reuse is fine.
                return Ok(());
            }
            None => {
                self.by_mangle.insert(mangled, ty.clone());
            }
        }
        match ty {
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => self.observe(e)?,
            Ty::Map(k, v) => {
                self.observe(k)?;
                self.observe(v)?;
            }
            Ty::Result_(t, e) => {
                self.observe(t)?;
                self.observe(e)?;
            }
            Ty::Tuple(ts) => {
                for t in ts {
                    self.observe(t)?;
                }
            }
            Ty::Func { params, ret } => {
                for p in params {
                    self.observe(p)?;
                }
                if let Some(r) = ret {
                    self.observe(r)?;
                }
            }
            // Records/enums: field types come from module decls, not from the
            // bare name node itself (matching backend_c TypeSet).
            Ty::Record(_) | Ty::Enum(_) => {}
            Ty::Int | Ty::Float | Ty::Bool | Ty::Infer(_) => {}
        }
        Ok(())
    }
}

/// Walk every type reachable in `modules` (declaration order, then the
/// given module slice order) and reject mangled-symbol collisions.
pub(crate) fn check_modules(modules: &[&IrModule]) -> Result<(), TypeError> {
    let mut map = CollisionMap::new();
    for m in modules {
        collect_module(m, &mut map)?;
    }
    Ok(())
}

fn collect_module(m: &IrModule, map: &mut CollisionMap) -> Result<(), TypeError> {
    for r in &m.records {
        map.observe(&Ty::Record(r.name.clone()))?;
        for f in &r.fields {
            map.observe(&f.ty)?;
        }
    }
    for e in &m.enums {
        map.observe(&Ty::Enum(e.name.clone()))?;
        for v in &e.variants {
            for f in &v.fields {
                map.observe(&f.ty)?;
            }
        }
    }
    for c in &m.consts {
        map.observe(&c.ty)?;
        walk_expr(&c.value, map)?;
    }
    for f in &m.funcs {
        for p in &f.params {
            map.observe(&p.ty)?;
        }
        if let Some(r) = &f.ret {
            map.observe(r)?;
        }
        walk_stmts(&f.body, map)?;
    }
    for t in &m.tests {
        walk_stmts(&t.body, map)?;
    }
    Ok(())
}

fn walk_stmts(stmts: &[IrStmt], map: &mut CollisionMap) -> Result<(), TypeError> {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                walk_place(target, map)?;
                walk_expr(value, map)?;
            }
            IrStmt::TupleAssign { value, .. } => walk_expr(value, map)?,
            IrStmt::Expr(e) => walk_expr(e, map)?,
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    walk_expr(c, map)?;
                    walk_stmts(b, map)?;
                }
                if let Some(b) = else_block {
                    walk_stmts(b, map)?;
                }
            }
            IrStmt::While { cond, body } => {
                walk_expr(cond, map)?;
                walk_stmts(body, map)?;
            }
            IrStmt::ForRange { from, to, body, .. } => {
                walk_expr(from, map)?;
                walk_expr(to, map)?;
                walk_stmts(body, map)?;
            }
            IrStmt::ForIn { iter, body, .. } => {
                walk_expr(iter, map)?;
                walk_stmts(body, map)?;
            }
            IrStmt::Match { scrutinee, arms } => {
                walk_expr(scrutinee, map)?;
                for a in arms {
                    walk_stmts(&a.body, map)?;
                }
            }
            IrStmt::Return(Some(e)) => walk_expr(e, map)?,
            IrStmt::Assert { cond, .. } => walk_expr(cond, map)?,
            IrStmt::ExpectTrap { body, .. } => walk_stmts(body, map)?,
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
    Ok(())
}

fn walk_expr(e: &IrExpr, map: &mut CollisionMap) -> Result<(), TypeError> {
    map.observe(&e.ty)?;
    match &e.kind {
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            for x in xs {
                walk_expr(x, map)?;
            }
        }
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, map)?;
            for x in args {
                walk_expr(x, map)?;
            }
        }
        IrExprKind::MutBuiltin { recv, recv_ty, args, .. } => {
            map.observe(recv_ty)?;
            walk_place(recv, map)?;
            for x in args {
                walk_expr(x, map)?;
            }
        }
        IrExprKind::GetField { recv, .. } => walk_expr(recv, map)?,
        IrExprKind::Index { recv, index } => {
            walk_expr(recv, map)?;
            walk_expr(index, map)?;
        }
        IrExprKind::Unary { operand, .. } => walk_expr(operand, map)?,
        IrExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, map)?;
            walk_expr(rhs, map)?;
        }
        _ => {}
    }
    Ok(())
}

fn walk_place(p: &Place, map: &mut CollisionMap) -> Result<(), TypeError> {
    match p {
        Place::Var(_) => Ok(()),
        Place::Index { base, base_ty, index } => {
            map.observe(base_ty)?;
            walk_place(base, map)?;
            walk_expr(index, map)
        }
        Place::Field { base, base_ty, .. } => {
            map.observe(base_ty)?;
            walk_place(base, map)
        }
    }
}
