//! Ownership facts for the copy-on-write backends (py, js).
//!
//! There every list, map, set and record is a handle on a reference-counted
//! box. Storing an aliasing read (`out.append(row)`, `return g`) shares the
//! box, and a write through a handle whose box has a second referent forks
//! it first. A share that nothing reads again is wasted, and it makes the
//! next write fork for nobody: `row = g[r]; row[c] = x; g[r] = row` copied
//! the whole row at every cell. [`analyze`] finds, per body:
//!
//! - **moves**: `Local` reads that are the local's last use. Storing one may
//!   hand over the handle itself instead of a share.
//! - **takes**: the `g[i]` of `x = g[i]` where the same block overwrites
//!   `g[i]` (same index) before anything else mentions `g`, with no jump in
//!   between. `x` may take the slot's own handle once `g` itself is unshared
//!   (`at_mut`): the old `g[i]` is never read again.
//!
//! Moves and takes are only sound for a local that holds its own handle. In
//! py and js a plain assignment stores a copy or a fresh value, and a
//! by-value parameter the body writes is copied at entry. Never-written
//! parameters, tuple-assignment targets and `for` / `match` binders can hold
//! a handle another live name also holds, and so can the root of a `for x in`
//! source, whose loop snapshot may hold its element handles uncounted. None
//! of those is moved or taken from. Inout parameters are live at every
//! return (the backend returns them), so they are never moved.

use std::collections::HashSet;

use crate::never_written::{expr_root_var, written_in_stmts};
use crate::{IrExpr, IrExprKind, IrModule, IrParam, IrPattern, IrStmt, Place, Ty};

/// The expressions of one body (by address) that [`analyze`] licenses.
#[derive(Default)]
pub struct Ownership {
    /// `Local` reads that are the local's last use: storing one moves it.
    pub moves: HashSet<*const IrExpr>,
    /// `g[i]` reads whose slot is dead: `x = g[i]` may take it.
    pub takes: HashSet<*const IrExpr>,
}

/// Ownership facts for a function (`params`, `body`) or a test (no params).
pub fn analyze<'a>(
    params: &'a [IrParam],
    body: &'a [IrStmt],
    m: &IrModule,
    all: &[IrModule],
) -> Ownership {
    let names = |inout: bool| params.iter().filter(move |p| p.inout == inout);
    let mut shared: Set = names(false)
        .filter(|p| p.never_written)
        .map(|p| p.name.as_str())
        .collect();
    let mut own = Ownership::default();
    let mut blocks = vec![body];
    walk(body, &mut |n| {
        if let Node::Stmt(s) = n {
            shared.extend(binders(s));
            if let IrStmt::ForIn { iter, .. } = s {
                shared.extend(expr_root_var(iter));
            }
            blocks.extend(child_blocks(s));
        }
    });
    let returned: Set = names(true).map(|p| p.name.as_str()).collect();
    let mut live = Live {
        shared: &shared,
        returned: &returned,
        moves: &mut own.moves,
        record: true,
    };
    live.block(body, returned.clone(), &Set::new(), &Set::new());
    for b in blocks {
        for i in 0..b.len() {
            let taken = take(&b[i..], &shared, m, all);
            own.takes.extend(taken.map(std::ptr::from_ref));
        }
    }
    own
}

type Set<'a> = HashSet<&'a str>;

/// Backwards liveness over one body, recording moves as it goes.
struct Live<'a, 'b> {
    shared: &'b Set<'a>,
    /// Live after every `return` (inout parameters).
    returned: &'b Set<'a>,
    moves: &'b mut HashSet<*const IrExpr>,
    /// Off while a loop's sets are still converging.
    record: bool,
}

impl<'a> Live<'a, '_> {
    /// Locals live before `stmts`, given those live after them and at the
    /// enclosing loop's exit (`brk`) and head (`cont`).
    fn block(
        &mut self,
        stmts: &'a [IrStmt],
        after: Set<'a>,
        brk: &Set<'a>,
        cont: &Set<'a>,
    ) -> Set<'a> {
        stmts
            .iter()
            .rev()
            .fold(after, |live, s| self.stmt(s, live, brk, cont))
    }

    fn stmt(&mut self, s: &'a IrStmt, after: Set<'a>, brk: &Set<'a>, cont: &Set<'a>) -> Set<'a> {
        let mut live = match s {
            IrStmt::Assign { .. }
            | IrStmt::TupleAssign { .. }
            | IrStmt::Expr(_)
            | IrStmt::Return(_) => return self.simple(s, after),
            IrStmt::If { arms, else_block } => {
                let mut live = match else_block {
                    Some(b) => self.block(b, after.clone(), brk, cont),
                    None => after.clone(),
                };
                for (_, b) in arms {
                    live.extend(self.block(b, after.clone(), brk, cont));
                }
                live
            }
            IrStmt::While { cond, body } => {
                self.looped(locals(|f| walk_expr(cond, f)), body, after)
            }
            IrStmt::ForRange { body, .. } | IrStmt::ForIn { body, .. } => {
                self.looped(Vec::new(), body, after)
            }
            IrStmt::Match { arms, .. } => arms.iter().fold(after.clone(), |mut live, a| {
                live.extend(self.block(&a.body, after.clone(), brk, cont));
                live
            }),
            IrStmt::ExpectTrap { body, .. } => self.block(body, after, brk, cont),
            IrStmt::Break => brk.clone(),
            IrStmt::Continue => cont.clone(),
            IrStmt::Assert { .. } | IrStmt::Skip => after,
        };
        // What the statement itself reads before its blocks: conditions,
        // range bounds, the `for` source, the scrutinee, the assertion.
        walk_own_exprs(s, &mut |e| live.extend(locals(|f| walk_expr(e, f))));
        live
    }

    /// A loop: iterate to the fixpoint, so a local the next iteration reads
    /// (or the `while` condition, `head`) is live at the end of the body.
    /// Moves are recorded only on a last pass over the converged sets.
    fn looped(&mut self, head: Vec<&'a str>, body: &'a [IrStmt], after: Set<'a>) -> Set<'a> {
        let record = std::mem::replace(&mut self.record, false);
        let mut live: Set = after.iter().copied().chain(head.iter().copied()).collect();
        loop {
            let mut next = self.block(body, live.clone(), &after, &live);
            next.extend(after.iter().chain(&head));
            if next == live {
                break;
            }
            live = next;
        }
        self.record = record;
        if record {
            self.block(body, live.clone(), &after, &live);
        }
        live
    }

    fn simple(&mut self, s: &'a IrStmt, after: Set<'a>) -> Set<'a> {
        let one = std::slice::from_ref(s);
        let after = if matches!(s, IrStmt::Return(_)) {
            self.returned.clone()
        } else {
            after
        };
        let mentions = locals(|f| walk(one, f));
        walk(one, &mut |n| {
            if let (true, Node::Expr(e)) = (self.record, n) {
                let last = |x| !after.contains(x) && !self.shared.contains(x);
                if local(e)
                    .is_some_and(|x| last(x) && mentions.iter().filter(|y| **y == x).count() == 1)
                {
                    self.moves.insert(e);
                }
            }
        });
        let (defs, reads) = match s {
            IrStmt::Assign {
                target: Place::Var(x),
                value,
                ..
            } => (vec![x.as_str()], locals(|f| walk_expr(value, f))),
            IrStmt::TupleAssign { targets, value, .. } => (
                targets.iter().map(String::as_str).collect(),
                locals(|f| walk_expr(value, f)),
            ),
            _ => (Vec::new(), mentions),
        };
        let mut live = after;
        for d in defs {
            live.remove(d);
        }
        live.extend(reads);
        live
    }
}

/// The `g[i]` of the `x = g[i]` at `rest[0]`, if `rest` overwrites `g[i]`
/// (`g[i] = v`, the same index, `v` not mentioning `g`) before anything else
/// mentions `g`, with no jump in between and no write to a local the index reads.
fn take<'a>(
    rest: &'a [IrStmt],
    shared: &Set,
    m: &IrModule,
    all: &[IrModule],
) -> Option<&'a IrExpr> {
    let IrStmt::Assign {
        target: Place::Var(_),
        value: e,
        ..
    } = &rest[0]
    else {
        return None;
    };
    let IrExprKind::Index { recv, index } = &e.kind else {
        return None;
    };
    let g = local(recv).filter(|g| matches!(recv.ty, Ty::List(_)) && !shared.contains(g))?;
    let mentions = |s: &[IrStmt]| {
        locals(|f| walk(s, f))
            .into_iter()
            .filter(|n| *n == g)
            .count()
    };
    let j = 1 + rest[1..]
        .iter()
        .position(|s| mentions(std::slice::from_ref(s)) > 0)?;
    let IrStmt::Assign {
        target:
            Place::Index {
                base,
                index: overwritten,
                ..
            },
        ..
    } = &rest[j]
    else {
        return None;
    };
    let mut jumps = false;
    walk(&rest[1..j], &mut |n| {
        jumps |= matches!(
            n,
            Node::Stmt(IrStmt::Break | IrStmt::Continue | IrStmt::Return(_))
        )
    });
    let written = written_in_stmts(&rest[..=j], m, all);
    let fixed = locals(|f| walk_expr(index, f))
        .iter()
        .all(|x| !written.contains(*x));
    let dead = matches!(&**base, Place::Var(b) if b == g)
        && overwritten == index
        && mentions(&rest[j..=j]) == 1;
    (dead && fixed && !jumps).then_some(e)
}

/// Names a statement binds besides plain assignment targets.
fn binders(s: &IrStmt) -> Vec<&str> {
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
fn child_blocks(s: &IrStmt) -> Vec<&[IrStmt]> {
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

fn local(e: &IrExpr) -> Option<&str> {
    match &e.kind {
        IrExprKind::Local(x) => Some(x),
        _ => None,
    }
}

/// Every name a walk mentions: reads, place roots and binders, with repeats.
fn locals<'a>(visit: impl FnOnce(&mut dyn FnMut(Node<'a>))) -> Vec<&'a str> {
    let mut out = Vec::new();
    visit(&mut |n| match n {
        Node::Stmt(s) => out.extend(binders(s)),
        Node::Expr(e) => out.extend(local(e)),
        Node::Root(x) => out.push(x),
    });
    out
}

enum Node<'a> {
    Stmt(&'a IrStmt),
    Expr(&'a IrExpr),
    /// The variable a place starts from.
    Root(&'a str),
}

/// Every statement, expression and place root in `stmts`, outermost first.
fn walk<'a>(stmts: &'a [IrStmt], f: &mut dyn FnMut(Node<'a>)) {
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
fn walk_own_exprs<'a>(s: &'a IrStmt, f: &mut dyn FnMut(&'a IrExpr)) {
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

fn walk_expr<'a>(e: &'a IrExpr, f: &mut dyn FnMut(Node<'a>)) {
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

fn walk_place<'a>(p: &'a Place, f: &mut dyn FnMut(Node<'a>)) {
    match p {
        Place::Var(n) => f(Node::Root(n)),
        Place::Index { base, index, .. } => {
            walk_place(base, f);
            walk_expr(index, f);
        }
        Place::Field { base, .. } => walk_place(base, f),
    }
}
