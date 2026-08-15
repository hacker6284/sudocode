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
        Self {
            by_mangle: BTreeMap::new(),
        }
    }

    /// Register `ty` and its component types. Returns an error if a different
    /// structural type already claimed this mangled string.
    fn observe(&mut self, ty: &Ty) -> Result<(), TypeError> {
        // Scalars cannot collide (length-prefixed leaf encodings). Empty
        // tuple is the checker's void sentinel and never materializes.
        // Infer never survives monomorphization; skip defensively.
        if is_scalar(ty)
            || matches!(ty, Ty::Tuple(ts) if ts.is_empty())
            || matches!(ty, Ty::Infer(_))
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
/// Also asserts no two record/enum decls share an IR symbol.
pub(crate) fn check_modules(modules: &[&IrModule]) -> Result<(), TypeError> {
    let mut seen_decls: BTreeMap<String, String> = BTreeMap::new();
    for m in modules {
        for r in &m.records {
            if let Some(prev) = seen_decls.insert(r.name.clone(), m.name.clone()) {
                return error(
                    0,
                    0,
                    format!(
                        "IR type symbol '{}' is declared in both '{prev}' and '{}' — \
                         the frontend uniquing pass must assign distinct symbols",
                        r.name, m.name
                    ),
                );
            }
        }
        for e in &m.enums {
            if let Some(prev) = seen_decls.insert(e.name.clone(), m.name.clone()) {
                return error(
                    0,
                    0,
                    format!(
                        "IR type symbol '{}' is declared in both '{prev}' and '{}' — \
                         the frontend uniquing pass must assign distinct symbols",
                        e.name, m.name
                    ),
                );
            }
        }
    }
    let mut map = CollisionMap::new();
    for m in modules {
        walk_module(m, &mut map)?;
    }
    Ok(())
}

/// Observer over every type the IR can name. Escape analysis and the
/// mangle-collision oracle share this walk so they cannot drift.
pub(crate) trait TySink {
    fn ty(&mut self, t: &Ty) -> Result<(), TypeError>;
}

impl TySink for CollisionMap {
    fn ty(&mut self, t: &Ty) -> Result<(), TypeError> {
        self.observe(t)
    }
}

/// Complete traversal of types reachable from `m` — decls, signatures,
/// bodies, places, and `IrPattern::Variant` enum names.
pub(crate) fn walk_module(m: &IrModule, sink: &mut impl TySink) -> Result<(), TypeError> {
    for r in &m.records {
        sink.ty(&Ty::Record(r.name.clone()))?;
        for f in &r.fields {
            sink.ty(&f.ty)?;
        }
    }
    for e in &m.enums {
        sink.ty(&Ty::Enum(e.name.clone()))?;
        for v in &e.variants {
            for f in &v.fields {
                sink.ty(&f.ty)?;
            }
        }
    }
    for c in &m.consts {
        sink.ty(&c.ty)?;
        walk_expr(&c.value, sink)?;
    }
    for f in &m.funcs {
        for p in &f.params {
            sink.ty(&p.ty)?;
        }
        if let Some(r) = &f.ret {
            sink.ty(r)?;
        }
        walk_stmts(&f.body, sink)?;
    }
    for t in &m.tests {
        walk_stmts(&t.body, sink)?;
    }
    Ok(())
}

fn walk_stmts(stmts: &[IrStmt], sink: &mut impl TySink) -> Result<(), TypeError> {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                walk_place(target, sink)?;
                walk_expr(value, sink)?;
            }
            IrStmt::TupleAssign { value, .. } => walk_expr(value, sink)?,
            IrStmt::Expr(e) => walk_expr(e, sink)?,
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    walk_expr(c, sink)?;
                    walk_stmts(b, sink)?;
                }
                if let Some(b) = else_block {
                    walk_stmts(b, sink)?;
                }
            }
            IrStmt::While { cond, body } => {
                walk_expr(cond, sink)?;
                walk_stmts(body, sink)?;
            }
            IrStmt::ForRange { from, to, body, .. } => {
                walk_expr(from, sink)?;
                walk_expr(to, sink)?;
                walk_stmts(body, sink)?;
            }
            IrStmt::ForIn { iter, body, .. } => {
                walk_expr(iter, sink)?;
                walk_stmts(body, sink)?;
            }
            IrStmt::Match { scrutinee, arms } => {
                walk_expr(scrutinee, sink)?;
                for a in arms {
                    if let sudoc_ir::IrPattern::Variant { enum_name, .. } = &a.pattern {
                        if enum_name != "Option" && enum_name != "Result" {
                            sink.ty(&Ty::Enum(enum_name.clone()))?;
                        }
                    }
                    walk_stmts(&a.body, sink)?;
                }
            }
            IrStmt::Return(Some(e)) => walk_expr(e, sink)?,
            IrStmt::Assert { cond, .. } => walk_expr(cond, sink)?,
            IrStmt::ExpectTrap { body, .. } => walk_stmts(body, sink)?,
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
    Ok(())
}

fn walk_expr(e: &IrExpr, sink: &mut impl TySink) -> Result<(), TypeError> {
    sink.ty(&e.ty)?;
    match &e.kind {
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => {
            for x in xs {
                walk_expr(x, sink)?;
            }
        }
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, sink)?;
            for x in args {
                walk_expr(x, sink)?;
            }
        }
        IrExprKind::MutBuiltin {
            recv,
            recv_ty,
            args,
            ..
        } => {
            sink.ty(recv_ty)?;
            walk_place(recv, sink)?;
            for x in args {
                walk_expr(x, sink)?;
            }
        }
        IrExprKind::GetField { recv, .. } => walk_expr(recv, sink)?,
        IrExprKind::Index { recv, index } => {
            walk_expr(recv, sink)?;
            walk_expr(index, sink)?;
        }
        IrExprKind::Unary { operand, .. } => walk_expr(operand, sink)?,
        IrExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, sink)?;
            walk_expr(rhs, sink)?;
        }
        _ => {}
    }
    Ok(())
}

fn walk_place(p: &Place, sink: &mut impl TySink) -> Result<(), TypeError> {
    match p {
        Place::Var(_) => Ok(()),
        Place::Index {
            base,
            base_ty,
            index,
        } => {
            sink.ty(base_ty)?;
            walk_place(base, sink)?;
            walk_expr(index, sink)
        }
        Place::Field { base, base_ty, .. } => {
            sink.ty(base_ty)?;
            walk_place(base, sink)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_oracle_list_3i64_vs_list_int() {
        // Construct the historical `record List_3i64` vs `List<int>` collision
        // directly — source programs can no longer reach the oracle, but the
        // defensive check must still detect it.
        let mut map = CollisionMap::new();
        let rec = Ty::Record("List_3i64".to_string());
        let list = Ty::List(Box::new(Ty::Int));
        assert!(map.observe(&rec).is_ok());
        assert!(map.observe(&list).is_err());
    }

    #[test]
    fn collision_oracle_map_underscore_ambiguity() {
        // Map<a_b, c> vs Map<a, b_c> both mangle to Map_a_b_c.
        let mut map = CollisionMap::new();
        let m1 = Ty::Map(
            Box::new(Ty::Record("a_b".into())),
            Box::new(Ty::Record("c".into())),
        );
        let m2 = Ty::Map(
            Box::new(Ty::Record("a".into())),
            Box::new(Ty::Record("b_c".into())),
        );
        assert!(map.observe(&m1).is_ok());
        assert!(map.observe(&m2).is_err());
    }

    #[test]
    fn collision_oracle_same_type_reuse_is_ok() {
        // Observing the same structural type twice is reuse, not a collision.
        let mut map = CollisionMap::new();
        let t1 = Ty::List(Box::new(Ty::Int));
        let t2 = Ty::List(Box::new(Ty::Int));
        assert!(map.observe(&t1).is_ok());
        assert!(map.observe(&t2).is_ok());
    }
}
