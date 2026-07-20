//! Swift type mapping, tuple-struct collection, and hashability.
//!
//! Unlike C, List/Map/Set/Option/Result are native generics — no per-
//! instantiation monomorphization. The only per-shape codegen is named
//! tuple structs (`TupN_<mangled>`) so tuples can conform to Equatable/
//! Hashable (Swift's anonymous tuples cannot).

use std::collections::{BTreeMap, HashSet};

use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrStmt, Place, Ty};

/// Mangled fragment for composite type names (matches backend_c).
pub(crate) fn mangle(ty: &Ty) -> String {
    match ty {
        Ty::Int => "i64".into(),
        Ty::Float => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::List(e) => format!("List_{}", mangle(e)),
        Ty::Set(e) => format!("Set_{}", mangle(e)),
        Ty::Map(k, v) => format!("Map_{}_{}", mangle(k), mangle(v)),
        Ty::Option_(e) => format!("Opt_{}", mangle(e)),
        Ty::Result_(t, e) => format!("Res_{}_{}", mangle(t), mangle(e)),
        Ty::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(mangle).collect();
            format!("Tup{}_{}", ts.len(), parts.join("_"))
        }
        Ty::Func { params, ret } => {
            let parts: Vec<String> = params.iter().map(mangle).collect();
            let r = ret.as_ref().map(|r| mangle(r)).unwrap_or_else(|| "void".into());
            format!("Fn_{}_to_{r}", parts.join("_"))
        }
        Ty::Record(n) | Ty::Enum(n) => n.clone(),
        Ty::Infer(_) => unreachable!("Infer escaped the checker"),
    }
}

/// Swift type used in declarations and annotations.
pub(crate) fn swift_type(ty: &Ty) -> String {
    match ty {
        Ty::Int => "Int64".into(),
        Ty::Float => "Double".into(),
        Ty::Bool => "Bool".into(),
        Ty::List(e) => format!("[{}]", swift_type(e)),
        Ty::Set(e) => format!("Set<{}>", swift_type(e)),
        Ty::Map(k, v) => format!("[{}: {}]", swift_type(k), swift_type(v)),
        Ty::Option_(e) => format!("SudoOption<{}>", swift_type(e)),
        Ty::Result_(t, e) => format!("SudoResult<{}, {}>", swift_type(t), swift_type(e)),
        Ty::Tuple(ts) => {
            if ts.is_empty() {
                "Void".into()
            } else {
                mangle(ty)
            }
        }
        Ty::Func { params, ret } => {
            let ps: Vec<String> = params.iter().map(swift_type).collect();
            let r = ret.as_ref().map(|r| swift_type(r)).unwrap_or_else(|| "Void".into());
            format!("({}) throws -> {r}", ps.join(", "))
        }
        Ty::Record(n) | Ty::Enum(n) => n.clone(),
        Ty::Infer(_) => unreachable!("Infer escaped the checker"),
    }
}

/// Distinct non-empty tuple shapes used in the module, keyed by mangled name.
pub(crate) fn collect_tuples(m: &IrModule) -> BTreeMap<String, Vec<Ty>> {
    let mut set = BTreeMap::new();
    for r in &m.records {
        for (_, t) in &r.fields {
            walk_ty(t, &mut set);
        }
    }
    for e in &m.enums {
        for v in &e.variants {
            for (_, t) in &v.fields {
                walk_ty(t, &mut set);
            }
        }
    }
    for c in &m.consts {
        walk_ty(&c.ty, &mut set);
        walk_expr(&c.value, &mut set);
    }
    for f in &m.funcs {
        for p in &f.params {
            walk_ty(&p.ty, &mut set);
        }
        if let Some(r) = &f.ret {
            walk_ty(r, &mut set);
        }
        walk_stmts(&f.body, &mut set);
    }
    for t in &m.tests {
        walk_stmts(&t.body, &mut set);
    }
    set
}

fn walk_ty(ty: &Ty, set: &mut BTreeMap<String, Vec<Ty>>) {
    match ty {
        Ty::Tuple(ts) if !ts.is_empty() => {
            let name = mangle(ty);
            if set.insert(name, ts.clone()).is_none() {
                for t in ts {
                    walk_ty(t, set);
                }
            }
        }
        Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => walk_ty(e, set),
        Ty::Map(k, v) | Ty::Result_(k, v) => {
            walk_ty(k, set);
            walk_ty(v, set);
        }
        Ty::Tuple(ts) => ts.iter().for_each(|t| walk_ty(t, set)),
        Ty::Func { params, ret } => {
            params.iter().for_each(|t| walk_ty(t, set));
            if let Some(r) = ret {
                walk_ty(r, set);
            }
        }
        _ => {}
    }
}

fn walk_stmts(stmts: &[IrStmt], set: &mut BTreeMap<String, Vec<Ty>>) {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                walk_place(target, set);
                walk_expr(value, set);
            }
            IrStmt::TupleAssign { value, .. } => walk_expr(value, set),
            IrStmt::Expr(e) => walk_expr(e, set),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    walk_expr(c, set);
                    walk_stmts(b, set);
                }
                if let Some(b) = else_block {
                    walk_stmts(b, set);
                }
            }
            IrStmt::While { cond, body } => {
                walk_expr(cond, set);
                walk_stmts(body, set);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                walk_expr(from, set);
                walk_expr(to, set);
                walk_stmts(body, set);
            }
            IrStmt::ForIn { iter, body, .. } => {
                walk_expr(iter, set);
                walk_stmts(body, set);
            }
            IrStmt::Match { scrutinee, arms } => {
                walk_expr(scrutinee, set);
                for a in arms {
                    walk_stmts(&a.body, set);
                }
            }
            IrStmt::Return(Some(e)) | IrStmt::Assert { cond: e, .. } => walk_expr(e, set),
            IrStmt::ExpectTrap { body, .. } => walk_stmts(body, set),
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
}

fn walk_place(p: &Place, set: &mut BTreeMap<String, Vec<Ty>>) {
    match p {
        Place::Var(_) => {}
        Place::Index { base, base_ty, index } => {
            walk_place(base, set);
            walk_ty(base_ty, set);
            walk_expr(index, set);
        }
        Place::Field { base, base_ty, .. } => {
            walk_place(base, set);
            walk_ty(base_ty, set);
        }
    }
}

fn walk_expr(e: &IrExpr, set: &mut BTreeMap<String, Vec<Ty>>) {
    walk_ty(&e.ty, set);
    match &e.kind {
        IrExprKind::List(xs) | IrExprKind::Tuple(xs) | IrExprKind::Builtin { args: xs, .. } => {
            xs.iter().for_each(|x| walk_expr(x, set));
        }
        IrExprKind::CallFunc { args, .. }
        | IrExprKind::NewRecord { args, .. }
        | IrExprKind::NewVariant { args, .. } => {
            args.iter().for_each(|a| walk_expr(a, set));
        }
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, set);
            args.iter().for_each(|a| walk_expr(a, set));
        }
        IrExprKind::MutBuiltin { recv, recv_ty, args, .. } => {
            walk_place(recv, set);
            walk_ty(recv_ty, set);
            args.iter().for_each(|a| walk_expr(a, set));
        }
        IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
            walk_expr(recv, set);
        }
        IrExprKind::Index { recv, index } => {
            walk_expr(recv, set);
            walk_expr(index, set);
        }
        IrExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, set);
            walk_expr(rhs, set);
        }
        _ => {}
    }
}

/// Spec §2.2 / types::is_hashable: int, bool, and composites of hashable types.
/// Coinductive on record/enum cycles (via Option etc.): currently-visiting
/// names are treated as hashable.
pub(crate) fn is_hashable(ty: &Ty, m: &IrModule, seen: &mut HashSet<String>) -> bool {
    match ty {
        Ty::Int | Ty::Bool => true,
        Ty::Float | Ty::Map(..) | Ty::Set(..) | Ty::Func { .. } | Ty::Infer(_) => false,
        Ty::List(t) | Ty::Option_(t) => is_hashable(t, m, seen),
        Ty::Result_(t, e) => is_hashable(t, m, seen) && is_hashable(e, m, seen),
        Ty::Tuple(ts) => ts.iter().all(|t| is_hashable(t, m, seen)),
        Ty::Record(name) => {
            if !seen.insert(name.clone()) {
                return true;
            }
            let ok = m
                .record(name)
                .is_some_and(|r| r.fields.iter().all(|(_, t)| is_hashable(t, m, seen)));
            seen.remove(name);
            ok
        }
        Ty::Enum(name) => {
            if !seen.insert(name.clone()) {
                return true;
            }
            let ok = m.enum_(name).is_some_and(|e| {
                e.variants
                    .iter()
                    .all(|v| v.fields.iter().all(|(_, t)| is_hashable(t, m, seen)))
            });
            seen.remove(name);
            ok
        }
    }
}

/// Conformances string for a type declaration: always Equatable; Hashable
/// when sudo-hashable.
pub(crate) fn conformances(ty: &Ty, m: &IrModule) -> String {
    if is_hashable(ty, m, &mut HashSet::new()) {
        ": Equatable, Hashable".into()
    } else {
        ": Equatable".into()
    }
}

/// Lower first letter only: `Dot` → `dot`, `RECT` → `rECT` (collision-free).
pub(crate) fn variant_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => {
            let mut s = c.to_lowercase().to_string();
            s.push_str(chars.as_str());
            s
        }
        None => name.to_string(),
    }
}

/// Backtick Swift keywords so user/variant identifiers stay valid.
pub(crate) fn swift_ident(name: &str) -> String {
    if is_swift_keyword(name) {
        format!("`{name}`")
    } else {
        name.to_string()
    }
}

fn is_swift_keyword(name: &str) -> bool {
    matches!(
        name,
        "associatedtype"
            | "class"
            | "deinit"
            | "enum"
            | "extension"
            | "fileprivate"
            | "func"
            | "import"
            | "init"
            | "inout"
            | "internal"
            | "let"
            | "open"
            | "operator"
            | "private"
            | "protocol"
            | "public"
            | "rethrows"
            | "static"
            | "struct"
            | "subscript"
            | "typealias"
            | "var"
            | "break"
            | "case"
            | "continue"
            | "default"
            | "defer"
            | "do"
            | "else"
            | "fallthrough"
            | "for"
            | "guard"
            | "if"
            | "in"
            | "repeat"
            | "return"
            | "switch"
            | "where"
            | "while"
            | "as"
            | "Any"
            | "catch"
            | "false"
            | "is"
            | "nil"
            | "super"
            | "self"
            | "Self"
            | "throw"
            | "throws"
            | "true"
            | "try"
            | "Type"
            | "type"
            | "precedencegroup"
            | "async"
            | "await"
            | "actor"
            | "nonisolated"
            | "isolated"
            | "borrowing"
            | "consuming"
            | "package"
            | "macro"
    )
}
