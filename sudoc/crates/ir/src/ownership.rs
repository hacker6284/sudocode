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
//! Both rest on one invariant: every container slot (local, list element,
//! map value, record field) owns its handle, so a dead slot can hand its
//! handle on. In py and js a plain assignment stores a copy or a fresh value,
//! and a by-value parameter the body writes is copied at entry. The
//! exceptions, which can hold a handle another live name also holds:
//!
//! - never-written parameters, tuple-assignment targets, `for` / `match`
//!   binders, and the root of a `for x in` source (its loop snapshot may hold
//!   its element handles uncounted). None of those is moved or taken from.
//! - py stores a tuple into a list element or record field without a copy
//!   (`dest_can_share_tuple`), so a tuple slot may share its handle. Takes are
//!   therefore limited to list, map, set and record elements, the only ones
//!   mutated in place and so the only ones a take helps. A backend that shares
//!   other slots must narrow these facts the same way.
//!
//! Inout parameters are live at every return (the backend returns them), so
//! they are never moved. Facts are keyed on `*const IrExpr`: the IR must not
//! be cloned or moved between [`analyze`] and emit.

use std::collections::HashSet;

use crate::never_written::{expr_root_var, written_in_stmts};
use crate::walk::{binders, child_blocks, local, locals, walk, walk_expr, walk_own_exprs, Node};
use crate::{IrExpr, IrExprKind, IrModule, IrParam, IrStmt, Place, Ty};

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
    // Only in-place-mutable elements own their handle in every backend.
    let owned = matches!(e.ty, Ty::List(_) | Ty::Map(..) | Ty::Set(_) | Ty::Record(_));
    let g =
        local(recv).filter(|g| owned && matches!(recv.ty, Ty::List(_)) && !shared.contains(g))?;
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
