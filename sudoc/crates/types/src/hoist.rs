//! Lowering pass: hoist inout-passing calls out of expressions (spec §5.2).
//!
//! After this pass, every CallFunc whose callee has inout parameters appears
//! only as the root of an `Assign` value or an `Expr` statement — the only
//! shapes backends handle (they emit the caller-side writeback there).
//!
//! Sequencing rules preserved:
//! - strict left-to-right evaluation: when a subexpression is hoisted, every
//!   impure sibling evaluated before it is hoisted too;
//! - `and`/`or` short-circuit: a right operand containing an inout call
//!   lowers to an `if` so it evaluates only when reached;
//! - `while` conditions re-evaluate per iteration: lowered to
//!   `while true { <cond stmts>; if not c { break }; body }`;
//! - `if`/`else if` chain conditions evaluate lazily: a chain whose later
//!   conditions need hoisting nests the rest inside `else`;
//! - assignment evaluates its RHS before target indices: the RHS is hoisted
//!   when the target place contains an inout call.

use std::collections::HashMap;

use sudoc_ir::{BinaryOp, IrExpr, IrExprKind, IrMatchArm, IrStmt, Place, Ty, UnaryOp};

/// Per-function inout flags for every function in the module.
pub(crate) type InoutFlags = HashMap<String, Vec<bool>>;

pub(crate) fn hoist_body(body: Vec<IrStmt>, flags: &InoutFlags) -> Vec<IrStmt> {
    let mut h = Hoister { flags, counter: 0 };
    h.block(body)
}

struct Hoister<'a> {
    flags: &'a InoutFlags,
    counter: u32,
}

impl Hoister<'_> {
    fn fresh(&mut self) -> String {
        let name = format!("_sudo_h{}", self.counter);
        self.counter += 1;
        name
    }

    fn callee_has_inout(&self, name: &str) -> bool {
        self.flags.get(name).is_some_and(|f| f.iter().any(|b| *b))
    }

    fn contains(&self, e: &IrExpr) -> bool {
        match &e.kind {
            IrExprKind::CallFunc { name, args } => {
                self.callee_has_inout(name) || args.iter().any(|a| self.contains(a))
            }
            IrExprKind::List(xs)
            | IrExprKind::Tuple(xs)
            | IrExprKind::NewRecord { args: xs, .. }
            | IrExprKind::NewVariant { args: xs, .. }
            | IrExprKind::Builtin { args: xs, .. } => xs.iter().any(|x| self.contains(x)),
            IrExprKind::CallValue { callee, args } => {
                self.contains(callee) || args.iter().any(|a| self.contains(a))
            }
            IrExprKind::MutBuiltin { recv, args, .. } => {
                self.contains_place(recv) || args.iter().any(|a| self.contains(a))
            }
            IrExprKind::GetField { recv, .. } => self.contains(recv),
            IrExprKind::Index { recv, index } => self.contains(recv) || self.contains(index),
            IrExprKind::Unary { operand, .. } => self.contains(operand),
            IrExprKind::Binary { lhs, rhs, .. } => self.contains(lhs) || self.contains(rhs),
            _ => false,
        }
    }

    fn contains_place(&self, p: &Place) -> bool {
        match p {
            Place::Var(_) => false,
            Place::Index { base, index, .. } => {
                self.contains_place(base) || self.contains(index)
            }
            Place::Field { base, .. } => self.contains_place(base),
        }
    }

    fn is_root_inout_call(&self, e: &IrExpr) -> bool {
        matches!(&e.kind, IrExprKind::CallFunc { name, .. } if self.callee_has_inout(name))
    }

    /// Effect-free and mutation-independent: safe to leave in place when a
    /// sibling to the right mutates.
    fn is_pure(e: &IrExpr) -> bool {
        matches!(
            e.kind,
            IrExprKind::Int(_)
                | IrExprKind::Float(_)
                | IrExprKind::Bool(_)
                | IrExprKind::Text(_)
                | IrExprKind::FuncRef(_)
        )
    }

    fn temp(&mut self, e: IrExpr, out: &mut Vec<IrStmt>) -> IrExpr {
        let name = self.fresh();
        let ty = e.ty.clone();
        out.push(IrStmt::Assign { target: Place::Var(name.clone()), value: e, declares: true });
        IrExpr { ty, kind: IrExprKind::Local(name) }
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self, stmts: Vec<IrStmt>) -> Vec<IrStmt> {
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, &mut out);
        }
        out
    }

    fn stmt(&mut self, s: IrStmt, out: &mut Vec<IrStmt>) {
        match s {
            IrStmt::Assign { target, value, declares } => {
                let place_has = self.contains_place(&target);
                let value = if self.is_root_inout_call(&value) {
                    self.linearize_root_call(value, out)
                } else if self.contains(&value) {
                    self.linearize(value, out)
                } else if place_has && !Self::is_pure(&value) {
                    // RHS evaluates before target indices (spec §5.1).
                    self.temp(value, out)
                } else {
                    value
                };
                let target = self.linearize_place(target, out);
                out.push(IrStmt::Assign { target, value, declares });
            }
            IrStmt::TupleAssign { targets, declares, value } => {
                let value = if self.contains(&value) {
                    self.linearize(value, out)
                } else {
                    value
                };
                out.push(IrStmt::TupleAssign { targets, declares, value });
            }
            IrStmt::Expr(e) => {
                let e = if self.is_root_inout_call(&e) {
                    self.linearize_root_call(e, out)
                } else if self.contains(&e) {
                    self.linearize(e, out)
                } else {
                    e
                };
                out.push(IrStmt::Expr(e));
            }
            IrStmt::If { arms, else_block } => self.lower_if(arms, else_block, out),
            IrStmt::While { cond, body } => {
                let body = self.block(body);
                if !self.contains(&cond) {
                    out.push(IrStmt::While { cond, body });
                    return;
                }
                let mut new_body = Vec::new();
                let c = self.linearize(cond, &mut new_body);
                let not_c = IrExpr {
                    ty: Ty::Bool,
                    kind: IrExprKind::Unary { op: UnaryOp::Not, operand: Box::new(c) },
                };
                new_body.push(IrStmt::If {
                    arms: vec![(not_c, vec![IrStmt::Break])],
                    else_block: None,
                });
                new_body.extend(body);
                out.push(IrStmt::While {
                    cond: IrExpr { ty: Ty::Bool, kind: IrExprKind::Bool(true) },
                    body: new_body,
                });
            }
            IrStmt::ForRange { var, from, to, down, body } => {
                let from =
                    if self.contains(&from) { self.linearize(from, out) } else { from };
                let to = if self.contains(&to) { self.linearize(to, out) } else { to };
                let body = self.block(body);
                out.push(IrStmt::ForRange { var, from, to, down, body });
            }
            IrStmt::ForIn { vars, iter, body } => {
                let iter =
                    if self.contains(&iter) { self.linearize(iter, out) } else { iter };
                let body = self.block(body);
                out.push(IrStmt::ForIn { vars, iter, body });
            }
            IrStmt::Match { scrutinee, arms } => {
                let scrutinee = if self.contains(&scrutinee) {
                    self.linearize(scrutinee, out)
                } else {
                    scrutinee
                };
                let arms = arms
                    .into_iter()
                    .map(|a| IrMatchArm { pattern: a.pattern, body: self.block(a.body) })
                    .collect();
                out.push(IrStmt::Match { scrutinee, arms });
            }
            IrStmt::Return(Some(e)) => {
                let e = if self.contains(&e) { self.linearize(e, out) } else { e };
                out.push(IrStmt::Return(Some(e)));
            }
            IrStmt::Assert { cond, line } => {
                let cond =
                    if self.contains(&cond) { self.linearize(cond, out) } else { cond };
                out.push(IrStmt::Assert { cond, line });
            }
            IrStmt::ExpectTrap { kind, body, line } => {
                out.push(IrStmt::ExpectTrap { kind, body: self.block(body), line });
            }
            other @ (IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue) => {
                out.push(other)
            }
        }
    }

    fn lower_if(
        &mut self,
        mut arms: Vec<(IrExpr, Vec<IrStmt>)>,
        else_block: Option<Vec<IrStmt>>,
        out: &mut Vec<IrStmt>,
    ) {
        let (cond, body) = arms.remove(0);
        let cond = if self.contains(&cond) { self.linearize(cond, out) } else { cond };
        let body = self.block(body);
        if arms.is_empty() {
            let else_block = else_block.map(|b| self.block(b));
            out.push(IrStmt::If { arms: vec![(cond, body)], else_block });
        } else if arms.iter().any(|(c, _)| self.contains(c)) {
            // Later conditions evaluate lazily: nest the rest in `else`.
            let mut nested = Vec::new();
            self.lower_if(arms, else_block, &mut nested);
            out.push(IrStmt::If { arms: vec![(cond, body)], else_block: Some(nested) });
        } else {
            let mut all = vec![(cond, body)];
            all.extend(arms.into_iter().map(|(c, b)| (c, self.block(b))));
            let else_block = else_block.map(|b| self.block(b));
            out.push(IrStmt::If { arms: all, else_block });
        }
    }

    // ---- expressions ------------------------------------------------------

    /// Reduce an inout-containing expression, emitting prefix statements in
    /// evaluation order; the returned expression is inout-call-free.
    fn linearize(&mut self, e: IrExpr, out: &mut Vec<IrStmt>) -> IrExpr {
        debug_assert!(self.contains(&e));
        let ty = e.ty.clone();
        match e.kind {
            IrExprKind::CallFunc { name, args } if self.callee_has_inout(&name) => {
                let call = self.linearize_root_call(
                    IrExpr { ty: ty.clone(), kind: IrExprKind::CallFunc { name, args } },
                    out,
                );
                self.temp(call, out)
            }
            IrExprKind::CallFunc { name, args } => {
                let args = self.siblings(args, out);
                IrExpr { ty, kind: IrExprKind::CallFunc { name, args } }
            }
            IrExprKind::Binary { op: op @ (BinaryOp::And | BinaryOp::Or), lhs, rhs } => {
                self.lower_shortcircuit(op, *lhs, *rhs, out)
            }
            IrExprKind::Binary { op, lhs, rhs } => {
                let mut pair = self.siblings(vec![*lhs, *rhs], out).into_iter();
                let (lhs, rhs) = (pair.next().unwrap(), pair.next().unwrap());
                IrExpr {
                    ty,
                    kind: IrExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
                }
            }
            IrExprKind::Unary { op, operand } => {
                let operand = self.linearize(*operand, out);
                IrExpr { ty, kind: IrExprKind::Unary { op, operand: Box::new(operand) } }
            }
            IrExprKind::List(xs) => {
                let xs = self.siblings(xs, out);
                IrExpr { ty, kind: IrExprKind::List(xs) }
            }
            IrExprKind::Tuple(xs) => {
                let xs = self.siblings(xs, out);
                IrExpr { ty, kind: IrExprKind::Tuple(xs) }
            }
            IrExprKind::NewRecord { name, args } => {
                let args = self.siblings(args, out);
                IrExpr { ty, kind: IrExprKind::NewRecord { name, args } }
            }
            IrExprKind::NewVariant { enum_name, variant, args } => {
                let args = self.siblings(args, out);
                IrExpr { ty, kind: IrExprKind::NewVariant { enum_name, variant, args } }
            }
            IrExprKind::Builtin { builtin, args } => {
                let args = self.siblings(args, out);
                IrExpr { ty, kind: IrExprKind::Builtin { builtin, args } }
            }
            IrExprKind::CallValue { callee, args } => {
                let mut all = vec![*callee];
                all.extend(args);
                let mut it = self.siblings(all, out).into_iter();
                let callee = Box::new(it.next().unwrap());
                IrExpr { ty, kind: IrExprKind::CallValue { callee, args: it.collect() } }
            }
            IrExprKind::MutBuiltin { builtin, recv, recv_ty, args } => {
                let recv = self.linearize_place(recv, out);
                let args = self.siblings(args, out);
                IrExpr { ty, kind: IrExprKind::MutBuiltin { builtin, recv, recv_ty, args } }
            }
            IrExprKind::GetField { recv, name } => {
                let recv = self.linearize(*recv, out);
                IrExpr { ty, kind: IrExprKind::GetField { recv: Box::new(recv), name } }
            }
            IrExprKind::Index { recv, index } => {
                let mut pair = self.siblings(vec![*recv, *index], out).into_iter();
                let (recv, index) = (pair.next().unwrap(), pair.next().unwrap());
                IrExpr {
                    ty,
                    kind: IrExprKind::Index {
                        recv: Box::new(recv),
                        index: Box::new(index),
                    },
                }
            }
            kind => IrExpr { ty, kind }, // leaves contain no calls
        }
    }

    /// Sibling rule: children evaluate left-to-right; everything impure that
    /// evaluates before the last inout-containing child is hoisted so it
    /// reads pre-mutation state (and traps in order).
    fn siblings(&mut self, children: Vec<IrExpr>, out: &mut Vec<IrStmt>) -> Vec<IrExpr> {
        let last = children.iter().rposition(|c| self.contains(c));
        let Some(last) = last else { return children };
        children
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                if i < last {
                    if self.contains(&c) {
                        self.linearize(c, out)
                    } else if Self::is_pure(&c) {
                        c
                    } else {
                        self.temp(c, out)
                    }
                } else if i == last {
                    self.linearize(c, out)
                } else {
                    c
                }
            })
            .collect()
    }

    /// A root-position inout call keeps its shape; only its non-inout
    /// arguments participate in the sibling rule (inout arguments are places,
    /// not value reads — hoisting them would break the writeback).
    fn linearize_root_call(&mut self, e: IrExpr, out: &mut Vec<IrStmt>) -> IrExpr {
        let IrExpr { ty, kind: IrExprKind::CallFunc { name, args } } = e else {
            unreachable!("caller checked shape")
        };
        let flags = self.flags[&name].clone();
        let last = args
            .iter()
            .zip(&flags)
            .rposition(|(a, inout)| !inout && self.contains(a));
        let args = match last {
            None => args,
            Some(last) => args
                .into_iter()
                .zip(flags)
                .enumerate()
                .map(|(i, (a, inout))| {
                    if inout {
                        a
                    } else if i < last {
                        if self.contains(&a) {
                            self.linearize(a, out)
                        } else if Self::is_pure(&a) {
                            a
                        } else {
                            self.temp(a, out)
                        }
                    } else if i == last {
                        self.linearize(a, out)
                    } else {
                        a
                    }
                })
                .collect(),
        };
        IrExpr { ty, kind: IrExprKind::CallFunc { name, args } }
    }

    fn linearize_place(&mut self, p: Place, out: &mut Vec<IrStmt>) -> Place {
        match p {
            Place::Var(n) => Place::Var(n),
            Place::Index { base, base_ty, index } => {
                let base = Box::new(self.linearize_place(*base, out));
                let index = if self.contains(&index) {
                    Box::new(self.linearize(*index, out))
                } else {
                    index
                };
                Place::Index { base, base_ty, index }
            }
            Place::Field { base, base_ty, name } => Place::Field {
                base: Box::new(self.linearize_place(*base, out)),
                base_ty,
                name,
            },
        }
    }

    /// `a and b` / `a or b` where `b` mutates: the right side must evaluate
    /// only when reached.
    fn lower_shortcircuit(
        &mut self,
        op: BinaryOp,
        lhs: IrExpr,
        rhs: IrExpr,
        out: &mut Vec<IrStmt>,
    ) -> IrExpr {
        let lhs = if self.contains(&lhs) { self.linearize(lhs, out) } else { lhs };
        if !self.contains(&rhs) {
            return IrExpr {
                ty: Ty::Bool,
                kind: IrExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
            };
        }
        // result = lhs; if [not] result { result = rhs }
        let result = self.fresh();
        out.push(IrStmt::Assign {
            target: Place::Var(result.clone()),
            value: lhs,
            declares: true,
        });
        let guard = {
            let read = IrExpr { ty: Ty::Bool, kind: IrExprKind::Local(result.clone()) };
            if op == BinaryOp::And {
                read
            } else {
                IrExpr {
                    ty: Ty::Bool,
                    kind: IrExprKind::Unary { op: UnaryOp::Not, operand: Box::new(read) },
                }
            }
        };
        let mut arm_body = Vec::new();
        let rhs = self.linearize(rhs, &mut arm_body);
        arm_body.push(IrStmt::Assign {
            target: Place::Var(result.clone()),
            value: rhs,
            declares: false,
        });
        out.push(IrStmt::If { arms: vec![(guard, arm_body)], else_block: None });
        IrExpr { ty: Ty::Bool, kind: IrExprKind::Local(result) }
    }
}
