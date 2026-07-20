//! Post-check pass: substitute all inference variables, reject anything
//! still unresolved, and run checks that need fully resolved types
//! (Map/Set key hashability, sortable element types).

use sudoc_ir::{Builtin, IrExpr, IrExprKind, IrStmt, Place, Ty};

use crate::{error, is_hashable, ModuleCtx, TypeError};

pub(crate) struct Finalizer<'a> {
    subst: &'a [Option<Ty>],
    ctx: &'a ModuleCtx,
    owner: &'a str,
    line: u32,
}

pub(crate) fn finalize_body(
    body: Vec<IrStmt>,
    subst: &[Option<Ty>],
    ctx: &ModuleCtx,
    owner: &str,
    line: u32,
) -> Result<Vec<IrStmt>, TypeError> {
    let f = Finalizer { subst, ctx, owner, line };
    body.into_iter().map(|s| f.stmt(s)).collect()
}

impl Finalizer<'_> {
    fn ty(&self, t: &Ty) -> Result<Ty, TypeError> {
        let resolved = self.resolve(t);
        if self.has_infer(&resolved) {
            return error(
                self.line,
                1,
                format!("cannot infer a type in '{}' (an empty [] / Map() / Set() / None is never constrained)", self.owner),
            );
        }
        self.validate(&resolved)?;
        Ok(resolved)
    }

    fn resolve(&self, t: &Ty) -> Ty {
        let mut head = t.clone();
        while let Ty::Infer(i) = head {
            match &self.subst[i as usize] {
                Some(b) => head = b.clone(),
                None => break,
            }
        }
        match head {
            Ty::List(e) => Ty::List(Box::new(self.resolve(&e))),
            Ty::Set(e) => Ty::Set(Box::new(self.resolve(&e))),
            Ty::Map(k, v) => Ty::Map(Box::new(self.resolve(&k)), Box::new(self.resolve(&v))),
            Ty::Option_(e) => Ty::Option_(Box::new(self.resolve(&e))),
            Ty::Result_(a, b) => Ty::Result_(Box::new(self.resolve(&a)), Box::new(self.resolve(&b))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Func { params, ret } => Ty::Func {
                params: params.iter().map(|t| self.resolve(t)).collect(),
                ret: ret.map(|r| Box::new(self.resolve(&r))),
            },
            other => other,
        }
    }

    fn has_infer(&self, t: &Ty) -> bool {
        match t {
            Ty::Infer(_) => true,
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => self.has_infer(e),
            Ty::Map(k, v) => self.has_infer(k) || self.has_infer(v),
            Ty::Result_(a, b) => self.has_infer(a) || self.has_infer(b),
            Ty::Tuple(ts) => ts.iter().any(|t| self.has_infer(t)),
            Ty::Func { params, ret } => {
                params.iter().any(|t| self.has_infer(t))
                    || ret.as_ref().is_some_and(|r| self.has_infer(r))
            }
            _ => false,
        }
    }

    /// Checks that need resolved types: hashability of inferred keys.
    fn validate(&self, t: &Ty) -> Result<(), TypeError> {
        match t {
            Ty::Map(k, v) => {
                if !is_hashable(k, Some(self.ctx), &mut Vec::new()) {
                    return error(
                        self.line,
                        1,
                        format!("Map key type {k} in '{}' is not hashable", self.owner),
                    );
                }
                self.validate(k)?;
                self.validate(v)
            }
            Ty::Set(e) => {
                if !is_hashable(e, Some(self.ctx), &mut Vec::new()) {
                    return error(
                        self.line,
                        1,
                        format!("Set element type {e} in '{}' is not hashable", self.owner),
                    );
                }
                self.validate(e)
            }
            Ty::List(e) | Ty::Option_(e) => self.validate(e),
            Ty::Result_(a, b) => {
                self.validate(a)?;
                self.validate(b)
            }
            Ty::Tuple(ts) => ts.iter().try_for_each(|t| self.validate(t)),
            Ty::Func { params, ret } => {
                params.iter().try_for_each(|t| self.validate(t))?;
                ret.as_ref().map_or(Ok(()), |r| self.validate(r))
            }
            _ => Ok(()),
        }
    }

    fn stmt(&self, s: IrStmt) -> Result<IrStmt, TypeError> {
        Ok(match s {
            IrStmt::Assign { target, value, declares } => IrStmt::Assign {
                target: self.place(target)?,
                value: self.expr(value)?,
                declares,
            },
            IrStmt::TupleAssign { targets, declares, value } => {
                IrStmt::TupleAssign { targets, declares, value: self.expr(value)? }
            }
            IrStmt::Expr(e) => IrStmt::Expr(self.expr_allow_void(e)?),
            IrStmt::If { arms, else_block } => IrStmt::If {
                arms: arms
                    .into_iter()
                    .map(|(c, b)| Ok((self.expr(c)?, self.block(b)?)))
                    .collect::<Result<_, TypeError>>()?,
                else_block: match else_block {
                    Some(b) => Some(self.block(b)?),
                    None => None,
                },
            },
            IrStmt::While { cond, body } => {
                IrStmt::While { cond: self.expr(cond)?, body: self.block(body)? }
            }
            IrStmt::ForRange { var, from, to, down, body } => IrStmt::ForRange {
                var,
                from: self.expr(from)?,
                to: self.expr(to)?,
                down,
                body: self.block(body)?,
            },
            IrStmt::ForIn { vars, iter, body } => {
                IrStmt::ForIn { vars, iter: self.expr(iter)?, body: self.block(body)? }
            }
            IrStmt::Match { scrutinee, arms } => IrStmt::Match {
                scrutinee: self.expr(scrutinee)?,
                arms: arms
                    .into_iter()
                    .map(|a| {
                        Ok(sudoc_ir::IrMatchArm { pattern: a.pattern, body: self.block(a.body)? })
                    })
                    .collect::<Result<_, TypeError>>()?,
            },
            IrStmt::Return(v) => IrStmt::Return(match v {
                Some(e) => Some(self.expr(e)?),
                None => None,
            }),
            IrStmt::Assert { cond, line } => IrStmt::Assert { cond: self.expr(cond)?, line },
            IrStmt::Skip => IrStmt::Skip,
            IrStmt::Break => IrStmt::Break,
            IrStmt::Continue => IrStmt::Continue,
            IrStmt::ExpectTrap { kind, body, line } => {
                IrStmt::ExpectTrap { kind, body: self.block(body)?, line }
            }
        })
    }

    fn block(&self, b: Vec<IrStmt>) -> Result<Vec<IrStmt>, TypeError> {
        b.into_iter().map(|s| self.stmt(s)).collect()
    }

    fn place(&self, p: Place) -> Result<Place, TypeError> {
        Ok(match p {
            Place::Var(n) => Place::Var(n),
            Place::Index { base, base_ty, index } => Place::Index {
                base: Box::new(self.place(*base)?),
                base_ty: self.ty(&base_ty)?,
                index: Box::new(self.expr(*index)?),
            },
            Place::Field { base, base_ty, name } => Place::Field {
                base: Box::new(self.place(*base)?),
                base_ty: self.ty(&base_ty)?,
                name,
            },
        })
    }

    fn expr(&self, e: IrExpr) -> Result<IrExpr, TypeError> {
        let ty = self.ty(&e.ty)?;
        self.expr_with(ty, e.kind)
    }

    /// Statement-position expressions may have the void sentinel type.
    fn expr_allow_void(&self, e: IrExpr) -> Result<IrExpr, TypeError> {
        let ty = if e.ty == Ty::Tuple(Vec::new()) {
            e.ty.clone()
        } else {
            self.ty(&e.ty)?
        };
        self.expr_with(ty, e.kind)
    }

    fn expr_with(&self, ty: Ty, kind: IrExprKind) -> Result<IrExpr, TypeError> {
        let kind = match kind {
            k @ (IrExprKind::Int(_)
            | IrExprKind::Float(_)
            | IrExprKind::Bool(_)
            | IrExprKind::Text(_)
            | IrExprKind::Local(_)
            | IrExprKind::Const(_)
            | IrExprKind::FuncRef(_)) => k,
            IrExprKind::List(xs) => {
                IrExprKind::List(xs.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?)
            }
            IrExprKind::Tuple(xs) => {
                IrExprKind::Tuple(xs.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?)
            }
            IrExprKind::CallFunc { name, args } => IrExprKind::CallFunc {
                name,
                args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
            },
            IrExprKind::CallValue { callee, args } => IrExprKind::CallValue {
                callee: Box::new(self.expr(*callee)?),
                args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
            },
            IrExprKind::NewRecord { name, args } => IrExprKind::NewRecord {
                name,
                args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
            },
            IrExprKind::NewVariant { enum_name, variant, args } => IrExprKind::NewVariant {
                enum_name,
                variant,
                args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
            },
            IrExprKind::Builtin { builtin, args } => IrExprKind::Builtin {
                builtin,
                args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
            },
            IrExprKind::MutBuiltin { builtin, recv, recv_ty, args } => {
                let recv_ty = self.ty(&recv_ty)?;
                if builtin == Builtin::ListSort {
                    match &recv_ty {
                        Ty::List(e) if matches!(**e, Ty::Int | Ty::Float) => {}
                        Ty::List(e) => {
                            return error(
                                self.line,
                                1,
                                format!("sort() supports List<int> and List<float> in v1, not List<{e}>"),
                            )
                        }
                        _ => {}
                    }
                }
                IrExprKind::MutBuiltin {
                    builtin,
                    recv: self.place(recv)?,
                    recv_ty,
                    args: args.into_iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?,
                }
            }
            IrExprKind::GetField { recv, name } => {
                IrExprKind::GetField { recv: Box::new(self.expr(*recv)?), name }
            }
            IrExprKind::Index { recv, index } => IrExprKind::Index {
                recv: Box::new(self.expr(*recv)?),
                index: Box::new(self.expr(*index)?),
            },
            IrExprKind::Unary { op, operand } => {
                IrExprKind::Unary { op, operand: Box::new(self.expr(*operand)?) }
            }
            IrExprKind::Binary { op, lhs, rhs } => IrExprKind::Binary {
                op,
                lhs: Box::new(self.expr(*lhs)?),
                rhs: Box::new(self.expr(*rhs)?),
            },
        };
        Ok(IrExpr { ty, kind })
    }
}
