//! One walker over IR bodies: every statement, expression and place root,
//! outermost first, plus the names each one mentions.

use crate::{IrExpr, IrExprKind, IrPattern, IrStmt, Place};

/// Names a statement binds besides plain assignment targets.
pub(crate) fn binders(s: &IrStmt) -> Vec<&str> {
    match s {
        IrStmt::TupleAssign { targets: names, .. } | IrStmt::ForIn { vars: names, .. } => {
            names.iter().map(String::as_str).collect()
        }
        IrStmt::ForRange { var, .. } => vec![var],
        IrStmt::Match { arms, .. } => arms
            .iter()
            .flat_map(|a| match &a.pattern {
                IrPattern::Variant { binders: names, .. } => {
                    names.iter().map(String::as_str).collect()
                }
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The blocks directly inside a statement.
pub(crate) fn child_blocks(s: &IrStmt) -> Vec<&[IrStmt]> {
    match s {
        IrStmt::If { arms, else_block } => arms
            .iter()
            .map(|(_, b)| b.as_slice())
            .chain(else_block.as_deref())
            .collect(),
        IrStmt::While { body, .. }
        | IrStmt::ForRange { body, .. }
        | IrStmt::ForIn { body, .. }
        | IrStmt::ExpectTrap { body, .. } => vec![body],
        IrStmt::Match { arms, .. } => arms.iter().map(|a| a.body.as_slice()).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn local(e: &IrExpr) -> Option<&str> {
    match &e.kind {
        IrExprKind::Local(x) => Some(x),
        _ => None,
    }
}

/// Every name a walk mentions: reads, place roots and binders, with repeats.
pub(crate) fn mentions<'a>(visit: impl FnOnce(&mut dyn FnMut(Node<'a>))) -> Vec<&'a str> {
    let mut out = Vec::new();
    visit(&mut |n| out.extend(mentions_of(n)));
    out
}

/// The names one node mentions.
pub(crate) fn mentions_of(n: Node<'_>) -> Vec<&str> {
    match n {
        Node::Stmt(s) => binders(s),
        Node::Expr(e) => local(e).into_iter().collect(),
        Node::Root(x) => vec![x],
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Stmt(&'a IrStmt),
    Expr(&'a IrExpr),
    /// The variable a place starts from.
    Root(&'a str),
}

/// Every statement, expression and place root in `stmts`, outermost first.
pub(crate) fn walk<'a>(stmts: &'a [IrStmt], f: &mut dyn FnMut(Node<'a>)) {
    for s in stmts {
        f(Node::Stmt(s));
        if let IrStmt::Assign { target, .. } = s {
            walk_place(target, f);
        }
        walk_own_exprs(s, &mut |e| walk_expr(e, f));
        for b in child_blocks(s) {
            walk(b, f);
        }
    }
}

/// The expressions a statement holds outside its blocks, in order.
pub(crate) fn walk_own_exprs<'a>(s: &'a IrStmt, f: &mut dyn FnMut(&'a IrExpr)) {
    match s {
        IrStmt::Assign { value: e, .. }
        | IrStmt::TupleAssign { value: e, .. }
        | IrStmt::Expr(e)
        | IrStmt::Return(Some(e))
        | IrStmt::Assert { cond: e, .. }
        | IrStmt::While { cond: e, .. }
        | IrStmt::ForIn { iter: e, .. }
        | IrStmt::Match { scrutinee: e, .. } => f(e),
        IrStmt::If { arms, .. } => arms.iter().for_each(|(c, _)| f(c)),
        IrStmt::ForRange { from, to, .. } => {
            f(from);
            f(to);
        }
        _ => {}
    }
}

pub(crate) fn walk_expr<'a>(e: &'a IrExpr, f: &mut dyn FnMut(Node<'a>)) {
    f(Node::Expr(e));
    match &e.kind {
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => xs.iter().for_each(|x| walk_expr(x, f)),
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, f);
            args.iter().for_each(|x| walk_expr(x, f));
        }
        IrExprKind::MutBuiltin { recv, args, .. } => {
            walk_place(recv, f);
            args.iter().for_each(|x| walk_expr(x, f));
        }
        IrExprKind::GetField { recv: x, .. } | IrExprKind::Unary { operand: x, .. } => {
            walk_expr(x, f)
        }
        IrExprKind::Index { recv: a, index: b } | IrExprKind::Binary { lhs: a, rhs: b, .. } => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        IrExprKind::Int(_)
        | IrExprKind::Float(_)
        | IrExprKind::Bool(_)
        | IrExprKind::Text(_)
        | IrExprKind::Local(_)
        | IrExprKind::Const(_)
        | IrExprKind::FuncRef(_) => {}
    }
}

pub(crate) fn walk_place<'a>(p: &'a Place, f: &mut dyn FnMut(Node<'a>)) {
    match p {
        Place::Var(n) => f(Node::Root(n)),
        Place::Index { base, index, .. } => {
            walk_place(base, f);
            walk_expr(index, f);
        }
        Place::Field { base, .. } => walk_place(base, f),
    }
}
