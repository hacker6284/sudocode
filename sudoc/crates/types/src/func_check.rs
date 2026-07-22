//! Function-body checking: local inference by unification, name resolution,
//! mutability rules, and lowering to typed IR.

use std::collections::HashMap;

use sudoc_ir::{
    Builtin, IrExpr, IrExprKind, IrFunc, IrMatchArm, IrParam, IrPattern, IrStmt, IrTest,
    Place, Ty,
};
use sudoc_syntax::ast::{self, BinaryOp, UnaryOp};

use crate::{check_name, error, finalize, ModuleCtx, TypeError};

/// Internal sentinel for "returns nothing" (unconstructible as a user type:
/// tuples have arity >= 2).
pub(crate) fn void() -> Ty {
    Ty::Tuple(Vec::new())
}

#[derive(Clone, Copy, PartialEq)]
enum LocalKind {
    Normal,
    LoopVar,
}

struct Local {
    ty: Ty,
    kind: LocalKind,
}

pub(crate) struct FnChecker<'a> {
    ctx: &'a ModuleCtx,
    subst: Vec<Option<Ty>>,
    scopes: Vec<HashMap<String, Local>>,
    ret: Option<Ty>,
    tmp_counter: u32,
    loop_depth: u32,
    in_test: bool,
}

pub(crate) fn check_func(
    f: &ast::FuncDecl,
    ctx: &ModuleCtx,
    type_names: &HashMap<String, bool>,
) -> Result<IrFunc, TypeError> {
    let mut ck = FnChecker::new(ctx);
    let sig = &ctx.funcs[&f.name];
    ck.ret = sig.ret.clone();
    ck.scopes.push(HashMap::new());
    let mut params = Vec::new();
    for (p, (ty, inout)) in f.params.iter().zip(&sig.params) {
        ck.declare(&p.name, ty.clone(), LocalKind::Normal, f.line)?;
        params.push(IrParam {
            name: p.name.clone(),
            inout: *inout,
            ty: ty.clone(),
            boundary: crate::type_expr_to_boundary_ty(&p.ty),
        });
    }
    let body = ck.check_block(&f.body)?;
    if sig.ret.is_some() && !definitely_returns(&body) {
        return error(
            f.line,
            1,
            format!("not every path through function '{}' returns a value", f.name),
        );
    }
    let body = finalize::finalize_body(body, &ck.subst, ctx, &f.name, f.line)?;
    let _ = type_names;
    Ok(IrFunc {
        name: f.name.clone(),
        export: f.export,
        params,
        ret: sig.ret.clone(),
        ret_boundary: f.ret.as_ref().map(crate::type_expr_to_boundary_ty),
        body,
    })
}

pub(crate) fn check_test(t: &ast::TestDecl, ctx: &ModuleCtx) -> Result<IrTest, TypeError> {
    let mut ck = FnChecker::new(ctx);
    ck.in_test = true;
    ck.scopes.push(HashMap::new());
    for (i, s) in t.body.iter().enumerate() {
        if matches!(s, ast::Stmt::ExpectTrap { .. }) && i + 1 != t.body.len() {
            return error(
                t.line,
                1,
                "expect_trap must be the final statement of its test (nothing observable runs after a trap)",
            );
        }
    }
    let body = ck.check_block(&t.body)?;
    let body = finalize::finalize_body(body, &ck.subst, ctx, &t.name, t.line)?;
    Ok(IrTest { name: t.name.clone(), body })
}

/// Module constants: scalar constant expressions only in v1, folded here so
/// overflow/division errors surface at compile time and every backend emits
/// an identical literal.
pub(crate) fn check_const_expr(
    e: &ast::Expr,
    ctx: &ModuleCtx,
) -> Result<(IrExpr, crate::ConstVal), TypeError> {
    use crate::ConstVal as V;
    fn go(e: &ast::Expr, ctx: &ModuleCtx) -> Result<V, TypeError> {
        let restriction = "module constants must be scalar constant expressions in v1";
        let ov = |line, col| TypeError {
            line,
            col,
            msg: "constant expression overflows a 64-bit int".into(),
        };
        Ok(match &e.kind {
            ast::ExprKind::Int(v) => V::I(*v),
            ast::ExprKind::Float(v) => V::F(*v),
            ast::ExprKind::Bool(v) => V::B(*v),
            ast::ExprKind::Var(name) => match ctx.const_vals.get(name) {
                Some(v) => *v,
                None => return error(e.line, e.col, format!("unknown constant '{name}'")),
            },
            ast::ExprKind::Unary { op: UnaryOp::Neg, operand } => match go(operand, ctx)? {
                V::I(v) => V::I(v.checked_neg().ok_or_else(|| ov(e.line, e.col))?),
                V::F(v) => V::F(-v),
                V::B(_) => return error(e.line, e.col, restriction),
            },
            ast::ExprKind::Binary { op, lhs, rhs } => {
                let (l, r) = (go(lhs, ctx)?, go(rhs, ctx)?);
                match (l, r) {
                    (V::I(a), V::I(b)) => V::I(match op {
                        BinaryOp::Add => a.checked_add(b).ok_or_else(|| ov(e.line, e.col))?,
                        BinaryOp::Sub => a.checked_sub(b).ok_or_else(|| ov(e.line, e.col))?,
                        BinaryOp::Mul => a.checked_mul(b).ok_or_else(|| ov(e.line, e.col))?,
                        BinaryOp::Div => {
                            if b == 0 {
                                return error(e.line, e.col, "constant expression divides by zero");
                            }
                            let q = a.checked_div(b).ok_or_else(|| ov(e.line, e.col))?;
                            if a % b != 0 && (a < 0) != (b < 0) {
                                q - 1
                            } else {
                                q
                            }
                        }
                        BinaryOp::Mod => {
                            if b == 0 {
                                return error(e.line, e.col, "constant expression divides by zero");
                            }
                            if b == -1 {
                                0
                            } else {
                                let r = a % b;
                                if r != 0 && (r < 0) != (b < 0) {
                                    r + b
                                } else {
                                    r
                                }
                            }
                        }
                        _ => return error(e.line, e.col, restriction),
                    }),
                    (V::F(a), V::F(b)) => V::F(match op {
                        BinaryOp::Add => a + b,
                        BinaryOp::Sub => a - b,
                        BinaryOp::Mul => a * b,
                        BinaryOp::Div => a / b,
                        _ => return error(e.line, e.col, restriction),
                    }),
                    _ => return error(e.line, e.col, restriction),
                }
            }
            _ => return error(e.line, e.col, restriction),
        })
    }
    let v = go(e, ctx)?;
    let ir = match v {
        V::I(x) => IrExpr { ty: Ty::Int, kind: IrExprKind::Int(x) },
        V::F(x) => IrExpr { ty: Ty::Float, kind: IrExprKind::Float(x) },
        V::B(x) => IrExpr { ty: Ty::Bool, kind: IrExprKind::Bool(x) },
    };
    Ok((ir, v))
}

/// Does this loop body contain a `break` that would exit *this* loop (as
/// opposed to a `break` belonging to a loop nested inside it)? Used by
/// `definitely_returns` to recognize `while true` without an escaping
/// `break` as a diverging (never-falls-through) statement.
fn contains_own_break(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Break => true,
        IrStmt::If { arms, else_block } => {
            arms.iter().any(|(_, b)| contains_own_break(b))
                || else_block.as_ref().is_some_and(|e| contains_own_break(e))
        }
        IrStmt::Match { arms, .. } => arms.iter().any(|a| contains_own_break(&a.body)),
        // A nested loop owns its own `break`s; they don't exit the outer loop.
        IrStmt::While { .. } | IrStmt::ForRange { .. } | IrStmt::ForIn { .. } => false,
        _ => false,
    })
}

/// Does every path through this block hit a `return`?
pub(crate) fn definitely_returns(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Return(_) => true,
        IrStmt::If { arms, else_block } => {
            else_block.as_ref().is_some_and(|e| definitely_returns(e))
                && arms.iter().all(|(_, b)| definitely_returns(b))
        }
        IrStmt::Match { arms, .. } => arms.iter().all(|a| definitely_returns(&a.body)),
        IrStmt::While { cond, body } => {
            matches!(cond.kind, IrExprKind::Bool(true)) && !contains_own_break(body)
        }
        _ => false,
    })
}

impl<'a> FnChecker<'a> {
    fn new(ctx: &'a ModuleCtx) -> Self {
        FnChecker {
            ctx,
            subst: Vec::new(),
            scopes: Vec::new(),
            ret: None,
            tmp_counter: 0,
            loop_depth: 0,
            in_test: false,
        }
    }

    // ---- inference machinery ---------------------------------------------

    fn fresh(&mut self) -> Ty {
        self.subst.push(None);
        Ty::Infer((self.subst.len() - 1) as u32)
    }

    /// Follow substitutions until the head is not a bound variable.
    fn shallow(&self, ty: &Ty) -> Ty {
        let mut t = ty.clone();
        while let Ty::Infer(i) = t {
            match &self.subst[i as usize] {
                Some(bound) => t = bound.clone(),
                None => break,
            }
        }
        t
    }

    /// Deep-resolve for error messages (unbound vars display as `?n`).
    fn resolved(&self, ty: &Ty) -> Ty {
        let t = self.shallow(ty);
        let map = |t: &Ty| self.resolved(t);
        match t {
            Ty::List(e) => Ty::List(Box::new(map(&e))),
            Ty::Set(e) => Ty::Set(Box::new(map(&e))),
            Ty::Map(k, v) => Ty::Map(Box::new(map(&k)), Box::new(map(&v))),
            Ty::Option_(e) => Ty::Option_(Box::new(map(&e))),
            Ty::Result_(a, b) => Ty::Result_(Box::new(map(&a)), Box::new(map(&b))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolved(t)).collect()),
            Ty::Func { params, ret } => Ty::Func {
                params: params.iter().map(|t| self.resolved(t)).collect(),
                ret: ret.as_ref().map(|r| Box::new(self.resolved(r))),
            },
            other => other,
        }
    }

    fn occurs(&self, var: u32, ty: &Ty) -> bool {
        match self.shallow(ty) {
            Ty::Infer(j) => j == var,
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => self.occurs(var, &e),
            Ty::Map(k, v) => self.occurs(var, &k) || self.occurs(var, &v),
            Ty::Result_(a, b) => self.occurs(var, &a) || self.occurs(var, &b),
            Ty::Tuple(ts) => ts.iter().any(|t| self.occurs(var, t)),
            Ty::Func { params, ret } => {
                params.iter().any(|t| self.occurs(var, t))
                    || ret.as_ref().is_some_and(|r| self.occurs(var, r))
            }
            _ => false,
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty, line: u32, col: u32) -> Result<(), TypeError> {
        let sa = self.shallow(a);
        let sb = self.shallow(b);
        match (&sa, &sb) {
            (Ty::Infer(i), _) => {
                if let Ty::Infer(j) = sb {
                    if *i == j {
                        return Ok(());
                    }
                }
                if self.occurs(*i, &sb) {
                    return error(line, col, "cannot infer a type (self-referential)");
                }
                self.subst[*i as usize] = Some(sb);
                Ok(())
            }
            (_, Ty::Infer(j)) => {
                if self.occurs(*j, &sa) {
                    return error(line, col, "cannot infer a type (self-referential)");
                }
                self.subst[*j as usize] = Some(sa);
                Ok(())
            }
            (Ty::Int, Ty::Int) | (Ty::Float, Ty::Float) | (Ty::Bool, Ty::Bool) => Ok(()),
            (Ty::List(x), Ty::List(y))
            | (Ty::Set(x), Ty::Set(y))
            | (Ty::Option_(x), Ty::Option_(y)) => self.unify(&x.clone(), &y.clone(), line, col),
            (Ty::Map(ka, va), Ty::Map(kb, vb)) => {
                self.unify(&ka.clone(), &kb.clone(), line, col)?;
                self.unify(&va.clone(), &vb.clone(), line, col)
            }
            (Ty::Result_(xa, ea), Ty::Result_(xb, eb)) => {
                self.unify(&xa.clone(), &xb.clone(), line, col)?;
                self.unify(&ea.clone(), &eb.clone(), line, col)
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.clone().iter().zip(ys.clone().iter()) {
                    self.unify(x, y, line, col)?;
                }
                Ok(())
            }
            (Ty::Func { params: pa, ret: ra }, Ty::Func { params: pb, ret: rb })
                if pa.len() == pb.len() =>
            {
                for (x, y) in pa.clone().iter().zip(pb.clone().iter()) {
                    self.unify(x, y, line, col)?;
                }
                match (ra.clone(), rb.clone()) {
                    (None, None) => Ok(()),
                    (Some(x), Some(y)) => self.unify(&x, &y, line, col),
                    _ => error(
                        line,
                        col,
                        format!(
                            "type mismatch: {} vs {}",
                            self.resolved(&sa),
                            self.resolved(&sb)
                        ),
                    ),
                }
            }
            (Ty::Record(x), Ty::Record(y)) | (Ty::Enum(x), Ty::Enum(y)) if x == y => Ok(()),
            _ => error(
                line,
                col,
                format!("type mismatch: {} vs {}", self.resolved(&sa), self.resolved(&sb)),
            ),
        }
    }

    // ---- scopes -----------------------------------------------------------

    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn declare(
        &mut self,
        name: &str,
        ty: Ty,
        kind: LocalKind,
        line: u32,
    ) -> Result<(), TypeError> {
        check_name(name, line)?;
        self.scopes
            .last_mut()
            .expect("scope stack never empty")
            .insert(name.to_string(), Local { ty, kind });
        Ok(())
    }

    // ---- statements -------------------------------------------------------

    fn check_block(&mut self, block: &[ast::Stmt]) -> Result<Vec<IrStmt>, TypeError> {
        self.scopes.push(HashMap::new());
        let mut out = Vec::new();
        for stmt in block {
            self.check_stmt(stmt, &mut out)?;
        }
        self.scopes.pop();
        Ok(out)
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt, out: &mut Vec<IrStmt>) -> Result<(), TypeError> {
        match stmt {
            ast::Stmt::Assign { targets, values, line } => {
                self.check_assign(targets, values, *line, out)
            }
            ast::Stmt::TypedAssign { name, ty, value, line } => {
                if self.lookup(name).is_some() {
                    return error(*line, 1, format!("'{name}' is already declared; annotations only appear on first assignment"));
                }
                let declared = self.surface_type(ty, *line)?;
                let v = self.check_expr(value)?;
                self.unify(&declared, &v.ty, *line, 1)?;
                self.declare(name, declared, LocalKind::Normal, *line)?;
                out.push(IrStmt::Assign {
                    target: Place::Var(name.clone()),
                    value: v,
                    declares: true,
                });
                Ok(())
            }
            ast::Stmt::Expr { expr, line } => {
                let ir = self.check_call_like(expr, true)?;
                let _ = line;
                out.push(IrStmt::Expr(ir));
                Ok(())
            }
            ast::Stmt::If { arms, else_block, line } => {
                let mut ir_arms = Vec::new();
                for (cond, body) in arms {
                    let c = self.check_expr(cond)?;
                    self.unify_msg(&c.ty, &Ty::Bool, *line, "if condition must be bool")?;
                    ir_arms.push((c, self.check_block(body)?));
                }
                let ir_else = match else_block {
                    Some(b) => Some(self.check_block(b)?),
                    None => None,
                };
                out.push(IrStmt::If { arms: ir_arms, else_block: ir_else });
                Ok(())
            }
            ast::Stmt::While { cond, body, line } => {
                let c = self.check_expr(cond)?;
                self.unify_msg(&c.ty, &Ty::Bool, *line, "while condition must be bool")?;
                self.loop_depth += 1;
                let b = self.check_block(body)?;
                self.loop_depth -= 1;
                out.push(IrStmt::While { cond: c, body: b });
                Ok(())
            }
            ast::Stmt::ForRange { var, from, to, down, body, line } => {
                let f = self.check_expr(from)?;
                self.unify_msg(&f.ty, &Ty::Int, *line, "for bounds must be int")?;
                let t = self.check_expr(to)?;
                self.unify_msg(&t.ty, &Ty::Int, *line, "for bounds must be int")?;
                self.scopes.push(HashMap::new());
                self.declare(var, Ty::Int, LocalKind::LoopVar, *line)?;
                self.loop_depth += 1;
                let mut b = Vec::new();
                for s in body {
                    self.check_stmt(s, &mut b)?;
                }
                self.loop_depth -= 1;
                self.scopes.pop();
                out.push(IrStmt::ForRange { var: var.clone(), from: f, to: t, down: *down, body: b });
                Ok(())
            }
            ast::Stmt::ForIn { vars, iter, body, line } => {
                let it = self.check_expr(iter)?;
                let var_tys: Vec<Ty> = match self.shallow(&it.ty) {
                    Ty::List(e) | Ty::Set(e) => {
                        if vars.len() != 1 {
                            return error(*line, 1, "iterating a List or Set binds exactly one variable");
                        }
                        vec![*e]
                    }
                    Ty::Map(k, v) => {
                        if vars.len() != 2 {
                            return error(*line, 1, "iterating a Map binds two variables: for key, value in m");
                        }
                        vec![*k, *v]
                    }
                    other => {
                        return error(*line, 1, format!("cannot iterate over {}", self.resolved(&other)))
                    }
                };
                self.scopes.push(HashMap::new());
                for (v, ty) in vars.iter().zip(var_tys) {
                    self.declare(v, ty, LocalKind::LoopVar, *line)?;
                }
                self.loop_depth += 1;
                let mut b = Vec::new();
                for s in body {
                    self.check_stmt(s, &mut b)?;
                }
                self.loop_depth -= 1;
                self.scopes.pop();
                out.push(IrStmt::ForIn { vars: vars.clone(), iter: it, body: b });
                Ok(())
            }
            ast::Stmt::Match { scrutinee, arms, line } => self.check_match(scrutinee, arms, *line, out),
            ast::Stmt::Return { value, line } => {
                let ir = match (value, self.ret.clone()) {
                    (None, None) => None,
                    (Some(_), None) => {
                        return error(*line, 1, "this function does not return a value")
                    }
                    (None, Some(_)) => {
                        return error(*line, 1, "return needs a value in this function")
                    }
                    (Some(v), Some(rt)) => {
                        let ir = self.check_expr(v)?;
                        self.unify(&rt, &ir.ty, *line, 1)?;
                        Some(ir)
                    }
                };
                out.push(IrStmt::Return(ir));
                Ok(())
            }
            ast::Stmt::Assert { cond, line } => {
                let c = self.check_expr(cond)?;
                self.unify_msg(&c.ty, &Ty::Bool, *line, "assert takes a bool")?;
                out.push(IrStmt::Assert { cond: c, line: *line });
                Ok(())
            }
            ast::Stmt::Skip { .. } => {
                out.push(IrStmt::Skip);
                Ok(())
            }
            ast::Stmt::Break { line } => {
                if self.loop_depth == 0 {
                    return error(*line, 1, "'break' outside of a loop");
                }
                out.push(IrStmt::Break);
                Ok(())
            }
            ast::Stmt::Continue { line } => {
                if self.loop_depth == 0 {
                    return error(*line, 1, "'continue' outside of a loop");
                }
                out.push(IrStmt::Continue);
                Ok(())
            }
            ast::Stmt::ExpectTrap { kind, body, line } => {
                if !self.in_test {
                    return error(*line, 1, "expect_trap is only allowed in test blocks");
                }
                const KINDS: &[&str] = &[
                    "OutOfBounds", "KeyMissing", "DivByZero", "Overflow",
                    "UnwrapFailed", "InvalidConvert", "InvalidArg", "AssertFailed",
                ];
                if kind == "StackOverflow" {
                    return error(*line, 1,
                        "'StackOverflow' is not an expectable trap kind — stack depth at overflow is non-deterministic across targets, so a lockstep test on it would be flaky"
                    );
                }
                if !KINDS.contains(&kind.as_str()) {
                    return error(*line, 1, format!(
                        "'{kind}' is not an expectable trap kind (one of: {})",
                        KINDS.join(", ")
                    ));
                }
                let b = self.check_block(body)?;
                out.push(IrStmt::ExpectTrap { kind: kind.clone(), body: b, line: *line });
                Ok(())
            }
        }
    }

    fn unify_msg(&mut self, a: &Ty, b: &Ty, line: u32, msg: &str) -> Result<(), TypeError> {
        self.unify(a, b, line, 1)
            .map_err(|e| TypeError { line, col: 1, msg: format!("{msg} (found {})", trim_mismatch(&e.msg)) })
    }

    fn check_assign(
        &mut self,
        targets: &[ast::Expr],
        values: &[ast::Expr],
        line: u32,
        out: &mut Vec<IrStmt>,
    ) -> Result<(), TypeError> {
        if targets.len() == 1 && values.len() == 1 {
            let v = self.check_expr(&values[0])?;
            let (place, declares) = self.assign_target(&targets[0], &v.ty, line)?;
            out.push(IrStmt::Assign { target: place, value: v, declares });
            return Ok(());
        }
        if targets.len() == values.len() {
            // Parallel assignment: RHS fully evaluated first, via temporaries.
            let mut temps = Vec::new();
            for value in values {
                let v = self.check_expr(value)?;
                let tmp = format!("_sudo_t{}", self.tmp_counter);
                self.tmp_counter += 1;
                temps.push((tmp.clone(), v.ty.clone()));
                out.push(IrStmt::Assign { target: Place::Var(tmp), value: v, declares: true });
            }
            for (target, (tmp, ty)) in targets.iter().zip(temps) {
                let (place, declares) = self.assign_target(target, &ty, line)?;
                out.push(IrStmt::Assign {
                    target: place,
                    value: IrExpr { ty, kind: IrExprKind::Local(tmp) },
                    declares,
                });
            }
            return Ok(());
        }
        if values.len() == 1 {
            // Tuple destructuring: x, y = f()
            let v = self.check_expr(&values[0])?;
            let elems = match self.shallow(&v.ty) {
                Ty::Tuple(ts) if ts.len() == targets.len() => ts,
                other => {
                    return error(
                        line,
                        1,
                        format!(
                            "destructuring {} variables needs a tuple of that arity, found {}",
                            targets.len(),
                            self.resolved(&other)
                        ),
                    )
                }
            };
            let mut names = Vec::new();
            let mut declares = Vec::new();
            for (t, ty) in targets.iter().zip(elems) {
                let name = match &t.kind {
                    ast::ExprKind::Var(n) => n.clone(),
                    _ => return error(line, 1, "tuple destructuring targets must be plain variables"),
                };
                let (_, d) = self.assign_target(t, &ty, line)?;
                names.push(name);
                declares.push(d);
            }
            out.push(IrStmt::TupleAssign { targets: names, declares, value: v });
            return Ok(());
        }
        error(line, 1, "assignment has mismatched numbers of targets and values")
    }

    /// Check an assignment target against the value type. Returns the place
    /// and whether this assignment declares a new variable.
    fn assign_target(
        &mut self,
        target: &ast::Expr,
        value_ty: &Ty,
        line: u32,
    ) -> Result<(Place, bool), TypeError> {
        if let ast::ExprKind::Var(name) = &target.kind {
            if let Some(local) = self.lookup(name) {
                if local.kind == LocalKind::LoopVar {
                    return error(line, 1, format!("cannot assign to loop variable '{name}'"));
                }
                let lty = local.ty.clone();
                self.unify(&lty, value_ty, line, target.col)?;
                return Ok((Place::Var(name.clone()), false));
            }
            if self.ctx.consts.contains_key(name) {
                return error(line, 1, format!("cannot assign to module constant '{name}'"));
            }
            self.declare(name, value_ty.clone(), LocalKind::Normal, line)?;
            return Ok((Place::Var(name.clone()), true));
        }
        let (place, pty) = self.check_place(target)?;
        self.unify(&pty, value_ty, line, target.col)?;
        Ok((place, false))
    }

    /// An assignable/mutable location: a chain of index/field steps rooted at
    /// a mutable local variable.
    fn check_place(&mut self, expr: &ast::Expr) -> Result<(Place, Ty), TypeError> {
        match &expr.kind {
            ast::ExprKind::Var(name) => match self.lookup(name) {
                Some(local) if local.kind == LocalKind::LoopVar => {
                    error(expr.line, expr.col, format!("cannot mutate loop variable '{name}'"))
                }
                Some(local) => Ok((Place::Var(name.clone()), local.ty.clone())),
                None => {
                    if self.ctx.consts.contains_key(name) {
                        error(expr.line, expr.col, format!("cannot mutate module constant '{name}'"))
                    } else {
                        error(expr.line, expr.col, format!("unknown variable '{name}'"))
                    }
                }
            },
            ast::ExprKind::Index { recv, index } => {
                let (base, bty) = self.check_place(recv)?;
                let idx = self.check_expr(index)?;
                let (elem, base_ty) = match self.shallow(&bty) {
                    Ty::List(e) => {
                        self.unify(&idx.ty, &Ty::Int, expr.line, expr.col)?;
                        (*e, Ty::List(Box::new(Ty::Infer(0)))) // placeholder head; fixed below
                    }
                    Ty::Map(k, v) => {
                        self.unify(&idx.ty, &k, expr.line, expr.col)?;
                        ((*v).clone(), Ty::Map(k.clone(), v))
                    }
                    other => {
                        return error(
                            expr.line,
                            expr.col,
                            format!("cannot index into {}", self.resolved(&other)),
                        )
                    }
                };
                // Store the real (possibly still-inferring) base type.
                let base_ty = match self.shallow(&bty) {
                    t @ (Ty::List(_) | Ty::Map(..)) => t,
                    _ => base_ty,
                };
                Ok((
                    Place::Index { base: Box::new(base), base_ty, index: Box::new(idx) },
                    elem,
                ))
            }
            ast::ExprKind::Field { recv, name } => {
                let (base, bty) = self.check_place(recv)?;
                match self.shallow(&bty) {
                    Ty::Record(rname) => {
                        let fields = &self.ctx.records[&rname];
                        match fields.iter().find(|(f, _)| f == name) {
                            Some((_, fty)) => Ok((
                                Place::Field {
                                    base: Box::new(base),
                                    base_ty: Ty::Record(rname.clone()),
                                    name: name.clone(),
                                },
                                fty.clone(),
                            )),
                            None => error(
                                expr.line,
                                expr.col,
                                format!("record {rname} has no field '{name}'"),
                            ),
                        }
                    }
                    other => error(
                        expr.line,
                        expr.col,
                        format!("{} has no assignable field '{name}'", self.resolved(&other)),
                    ),
                }
            }
            _ => error(expr.line, expr.col, "this expression is not assignable"),
        }
    }

    fn check_match(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        line: u32,
        out: &mut Vec<IrStmt>,
    ) -> Result<(), TypeError> {
        let scrut = self.check_expr(scrutinee)?;
        let sty = self.shallow(&scrut.ty);

        // (enum name, variants as (name, field types)) for variant-shaped types.
        type VariantShapes = Vec<(String, Vec<Ty>)>;
        let enum_info: Option<(String, VariantShapes)> = match &sty {
            Ty::Enum(name) => Some((
                name.clone(),
                self.ctx.enums[name]
                    .iter()
                    .map(|v| (v.name.clone(), v.fields.iter().map(|(_, t)| t.clone()).collect()))
                    .collect(),
            )),
            Ty::Option_(t) => Some((
                "Option".to_string(),
                vec![("Some".into(), vec![(**t).clone()]), ("None".into(), vec![])],
            )),
            Ty::Result_(t, e) => Some((
                "Result".to_string(),
                vec![("Ok".into(), vec![(**t).clone()]), ("Err".into(), vec![(**e).clone()])],
            )),
            Ty::Int | Ty::Bool => None,
            other => {
                return error(line, 1, format!("cannot match on {}", self.resolved(other)))
            }
        };

        let mut ir_arms = Vec::new();
        let mut covered: Vec<String> = Vec::new();
        let mut saw_wildcard = false;
        let mut bools_covered = [false, false];
        for arm in arms {
            if saw_wildcard {
                return error(arm.line, 1, "cases after 'case _' are unreachable");
            }
            let pattern = match (&arm.pattern, &enum_info) {
                (ast::Pattern::Wildcard, _) => {
                    saw_wildcard = true;
                    IrPattern::Wildcard
                }
                (ast::Pattern::Int(v), None) if sty == Ty::Int => IrPattern::Int(*v),
                (ast::Pattern::Bool(v), None) if sty == Ty::Bool => {
                    bools_covered[*v as usize] = true;
                    IrPattern::Bool(*v)
                }
                (ast::Pattern::Variant { qualifier, name, binders }, Some((ename, variants))) => {
                    if let Some(q) = qualifier {
                        if q != ename {
                            return error(arm.line, 1, format!("'{q}.{name}' does not belong to {ename}"));
                        }
                    }
                    let Some((_, field_tys)) = variants.iter().find(|(v, _)| v == name) else {
                        return error(arm.line, 1, format!("'{name}' is not a variant of {ename}"));
                    };
                    if binders.len() != field_tys.len() {
                        return error(
                            arm.line,
                            1,
                            format!(
                                "variant {name} has {} field(s), but the pattern binds {}",
                                field_tys.len(),
                                binders.len()
                            ),
                        );
                    }
                    if covered.contains(name) {
                        return error(arm.line, 1, format!("variant {name} is matched twice"));
                    }
                    covered.push(name.clone());
                    IrPattern::Variant {
                        enum_name: ename.clone(),
                        variant: name.clone(),
                        binders: binders.clone(),
                    }
                }
                (p, _) => {
                    return error(
                        arm.line,
                        1,
                        format!("pattern {p:?} does not fit a match on {}", self.resolved(&sty)),
                    )
                }
            };
            // Bind pattern variables in the arm's scope.
            self.scopes.push(HashMap::new());
            if let IrPattern::Variant { enum_name, variant, binders } = &pattern {
                let field_tys: Vec<Ty> = enum_info
                    .as_ref()
                    .and_then(|(_, vs)| vs.iter().find(|(v, _)| v == variant))
                    .map(|(_, ts)| ts.clone())
                    .unwrap_or_default();
                let _ = enum_name;
                for (b, ty) in binders.iter().zip(field_tys) {
                    self.declare(b, ty, LocalKind::Normal, arm.line)?;
                }
            }
            let mut body = Vec::new();
            for s in &arm.body {
                self.check_stmt(s, &mut body)?;
            }
            self.scopes.pop();
            ir_arms.push(IrMatchArm { pattern, body });
        }

        // Exhaustiveness.
        if !saw_wildcard {
            match (&sty, &enum_info) {
                (Ty::Int, _) => {
                    return error(line, 1, "match on int is not exhaustive: add a 'case _'")
                }
                (Ty::Bool, _) => {
                    if !(bools_covered[0] && bools_covered[1]) {
                        return error(line, 1, "match on bool is not exhaustive");
                    }
                }
                (_, Some((ename, variants))) => {
                    let missing: Vec<&str> = variants
                        .iter()
                        .map(|(v, _)| v.as_str())
                        .filter(|v| !covered.iter().any(|c| c == v))
                        .collect();
                    if !missing.is_empty() {
                        return error(
                            line,
                            1,
                            format!(
                                "match on {ename} is not exhaustive: missing {}",
                                missing.join(", ")
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
        out.push(IrStmt::Match { scrutinee: scrut, arms: ir_arms });
        Ok(())
    }

    fn surface_type(&self, t: &ast::TypeExpr, line: u32) -> Result<Ty, TypeError> {
        // Reuse module-level resolution; type names table reconstructed from ctx.
        let mut names = HashMap::new();
        for name in self.ctx.records.keys() {
            names.insert(name.clone(), true);
        }
        for name in self.ctx.enums.keys() {
            names.insert(name.clone(), false);
        }
        crate::resolve_type(t, &names, line)
    }

    // ---- expressions ------------------------------------------------------

    fn check_expr(&mut self, e: &ast::Expr) -> Result<IrExpr, TypeError> {
        let ir = self.check_call_like(e, false)?;
        Ok(ir)
    }

    /// `allow_void`: statement position, where value-less calls are legal.
    fn check_call_like(&mut self, e: &ast::Expr, allow_void: bool) -> Result<IrExpr, TypeError> {
        let ir = self.check_expr_inner(e)?;
        if !allow_void && ir.ty == void() {
            return error(e.line, e.col, "this call returns nothing and cannot be used in an expression");
        }
        Ok(ir)
    }

    fn check_expr_inner(&mut self, e: &ast::Expr) -> Result<IrExpr, TypeError> {
        let (line, col) = (e.line, e.col);
        match &e.kind {
            ast::ExprKind::Int(v) => Ok(IrExpr { ty: Ty::Int, kind: IrExprKind::Int(*v) }),
            ast::ExprKind::Float(v) => Ok(IrExpr { ty: Ty::Float, kind: IrExprKind::Float(*v) }),
            ast::ExprKind::Bool(v) => Ok(IrExpr { ty: Ty::Bool, kind: IrExprKind::Bool(*v) }),
            ast::ExprKind::Text(s) => {
                Ok(IrExpr { ty: Ty::list(Ty::Int), kind: IrExprKind::Text(s.clone()) })
            }
            ast::ExprKind::Var(name) => self.check_var(name, line, col),
            ast::ExprKind::ListLit(items) => {
                let elem = self.fresh();
                let mut irs = Vec::new();
                for item in items {
                    let ir = self.check_expr(item)?;
                    self.unify(&elem, &ir.ty, item.line, item.col)?;
                    irs.push(ir);
                }
                Ok(IrExpr { ty: Ty::List(Box::new(elem)), kind: IrExprKind::List(irs) })
            }
            ast::ExprKind::TupleLit(items) => {
                let mut irs = Vec::new();
                let mut tys = Vec::new();
                for item in items {
                    let ir = self.check_expr(item)?;
                    tys.push(ir.ty.clone());
                    irs.push(ir);
                }
                Ok(IrExpr { ty: Ty::Tuple(tys), kind: IrExprKind::Tuple(irs) })
            }
            ast::ExprKind::Unary { op, operand } => {
                let ir = self.check_expr(operand)?;
                let ty = match op {
                    UnaryOp::Neg => match self.shallow(&ir.ty) {
                        Ty::Int => Ty::Int,
                        Ty::Float => Ty::Float,
                        other => {
                            return error(
                                line,
                                col,
                                format!("unary '-' needs int or float, found {}", self.resolved(&other)),
                            )
                        }
                    },
                    UnaryOp::Not => {
                        self.unify(&ir.ty, &Ty::Bool, line, col)?;
                        Ty::Bool
                    }
                };
                Ok(IrExpr { ty, kind: IrExprKind::Unary { op: *op, operand: Box::new(ir) } })
            }
            ast::ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, line, col),
            ast::ExprKind::Index { recv, index } => {
                let r = self.check_expr(recv)?;
                let idx = self.check_expr(index)?;
                let ty = match self.shallow(&r.ty) {
                    Ty::List(elem) => {
                        self.unify(&idx.ty, &Ty::Int, line, col)?;
                        *elem
                    }
                    Ty::Map(k, v) => {
                        self.unify(&idx.ty, &k, line, col)?;
                        *v
                    }
                    other => {
                        return error(line, col, format!("cannot index into {}", self.resolved(&other)))
                    }
                };
                Ok(IrExpr {
                    ty,
                    kind: IrExprKind::Index { recv: Box::new(r), index: Box::new(idx) },
                })
            }
            ast::ExprKind::Field { recv, name } => {
                if let ast::ExprKind::Var(m) = &recv.kind {
                    // Qualified enum variant construction: `Enum.Variant`.
                    if self.lookup(m).is_none() && self.ctx.enums.contains_key(m) {
                        let variant = self.ctx.enums[m].iter().find(|v| &v.name == name);
                        return match variant {
                            Some(v) if v.fields.is_empty() => Ok(IrExpr {
                                ty: Ty::Enum(m.clone()),
                                kind: IrExprKind::NewVariant {
                                    enum_name: m.clone(),
                                    variant: name.clone(),
                                    args: vec![],
                                },
                            }),
                            Some(_) => error(line, col, format!("variant {name} needs arguments")),
                            None => error(line, col, format!("enum {m} has no variant '{name}'")),
                        };
                    }
                    if self.lookup(m).is_none() && self.ctx.deps.contains_key(m) {
                        let dep = &self.ctx.deps[m];
                        if let Some(ty) = dep.consts.get(name) {
                            return Ok(IrExpr {
                                ty: ty.clone(),
                                kind: IrExprKind::Const(format!("{m}.{name}")),
                            });
                        }
                        if dep.funcs.contains_key(name) || dep.generics.contains_key(name) {
                            return error(line, col, format!("'{m}.{name}' is a function — call it with ()"));
                        }
                        return error(line, col, format!("module '{m}' has no constant '{name}'"));
                    }
                }
                let r = self.check_expr(recv)?;
                match self.shallow(&r.ty) {
                    Ty::List(_) if name == "length" => Ok(IrExpr {
                        ty: Ty::Int,
                        kind: IrExprKind::Builtin { builtin: Builtin::ListLength, args: vec![r] },
                    }),
                    Ty::Map(..) if name == "size" => Ok(IrExpr {
                        ty: Ty::Int,
                        kind: IrExprKind::Builtin { builtin: Builtin::MapSize, args: vec![r] },
                    }),
                    Ty::Set(_) if name == "size" => Ok(IrExpr {
                        ty: Ty::Int,
                        kind: IrExprKind::Builtin { builtin: Builtin::SetSize, args: vec![r] },
                    }),
                    Ty::Record(rname) => {
                        let fields = &self.ctx.records[&rname];
                        match fields.iter().find(|(f, _)| f == name) {
                            Some((_, fty)) => Ok(IrExpr {
                                ty: fty.clone(),
                                kind: IrExprKind::GetField { recv: Box::new(r), name: name.clone() },
                            }),
                            None => error(line, col, format!("record {rname} has no field '{name}'")),
                        }
                    }
                    other => error(
                        line,
                        col,
                        format!("{} has no property '{name}'", self.resolved(&other)),
                    ),
                }
            }
            ast::ExprKind::Call { callee, args } => self.check_call(callee, args, line, col),
        }
    }

    fn check_var(&mut self, name: &str, line: u32, col: u32) -> Result<IrExpr, TypeError> {
        if let Some(local) = self.lookup(name) {
            return Ok(IrExpr { ty: local.ty.clone(), kind: IrExprKind::Local(name.to_string()) });
        }
        if let Some(ty) = self.ctx.consts.get(name) {
            return Ok(IrExpr { ty: ty.clone(), kind: IrExprKind::Const(name.to_string()) });
        }
        if name == "None" {
            let t = self.fresh();
            return Ok(IrExpr {
                ty: Ty::Option_(Box::new(t)),
                kind: IrExprKind::NewVariant {
                    enum_name: "Option".into(),
                    variant: "None".into(),
                    args: vec![],
                },
            });
        }
        if let Some(enums) = self.ctx.variants.get(name) {
            if enums.len() > 1 {
                return error(
                    line,
                    col,
                    format!("variant '{name}' is ambiguous ({}); qualify it", enums.join(", ")),
                );
            }
            let ename = enums[0].clone();
            let variant = self.ctx.enums[&ename].iter().find(|v| v.name == name).unwrap();
            if !variant.fields.is_empty() {
                return error(line, col, format!("variant {name} needs arguments"));
            }
            return Ok(IrExpr {
                ty: Ty::Enum(ename.clone()),
                kind: IrExprKind::NewVariant { enum_name: ename, variant: name.to_string(), args: vec![] },
            });
        }
        if let Some(sig) = self.ctx.funcs.get(name) {
            return Ok(IrExpr {
                ty: Ty::Func {
                    params: sig.params.iter().map(|(t, _)| t.clone()).collect(),
                    ret: sig.ret.clone().map(Box::new),
                },
                kind: IrExprKind::FuncRef(name.to_string()),
            });
        }
        if self.ctx.generics.contains_key(name) {
            return error(
                line,
                col,
                format!("generic function '{name}' cannot be used as a value; call it directly with concrete arguments"),
            );
        }
        if self.ctx.deps.contains_key(name) {
            return error(line, col, format!("'{name}' is a module; use {name}.something"));
        }
        error(line, col, format!("unknown variable '{name}'"))
    }

    fn check_binary(
        &mut self,
        op: BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        let l = self.check_expr(lhs)?;
        let r = self.check_expr(rhs)?;
        let ty = match op {
            BinaryOp::Add => match self.shallow(&l.ty) {
                Ty::Int => {
                    self.unify(&r.ty, &Ty::Int, line, col)?;
                    Ty::Int
                }
                Ty::Float => {
                    self.unify(&r.ty, &Ty::Float, line, col)?;
                    Ty::Float
                }
                Ty::List(_) | Ty::Infer(_) => {
                    self.unify(&l.ty, &r.ty, line, col)?;
                    match self.shallow(&l.ty) {
                        t @ (Ty::Int | Ty::Float | Ty::List(_)) => t,
                        other => {
                            return error(
                                line,
                                col,
                                format!("'+' needs int, float, or List operands, found {}", self.resolved(&other)),
                            )
                        }
                    }
                }
                other => {
                    return error(
                        line,
                        col,
                        format!("'+' needs int, float, or List operands, found {}", self.resolved(&other)),
                    )
                }
            },
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
                match self.shallow(&l.ty) {
                    Ty::Float => {
                        self.unify(&r.ty, &Ty::Float, line, col)?;
                        Ty::Float
                    }
                    _ => {
                        self.unify(&l.ty, &Ty::Int, line, col)?;
                        self.unify(&r.ty, &Ty::Int, line, col)?;
                        Ty::Int
                    }
                }
            }
            BinaryOp::Mod => {
                self.unify(&l.ty, &Ty::Int, line, col)?;
                self.unify(&r.ty, &Ty::Int, line, col)?;
                Ty::Int
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                match self.shallow(&l.ty) {
                    Ty::Int => self.unify(&r.ty, &Ty::Int, line, col)?,
                    Ty::Float => self.unify(&r.ty, &Ty::Float, line, col)?,
                    other => {
                        let sym = match op {
                            BinaryOp::Lt => "<",
                            BinaryOp::Le => "<=",
                            BinaryOp::Gt => ">",
                            _ => ">=",
                        };
                        return error(
                            line,
                            col,
                            format!("operator '{sym}' requires int or float operands, found {}", self.resolved(&other)),
                        );
                    }
                }
                Ty::Bool
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                self.unify(&l.ty, &r.ty, line, col)?;
                if contains_func(&self.resolved(&l.ty)) {
                    return error(line, col, "function values cannot be compared");
                }
                Ty::Bool
            }
            BinaryOp::And | BinaryOp::Or => {
                self.unify(&l.ty, &Ty::Bool, line, col)?;
                self.unify(&r.ty, &Ty::Bool, line, col)?;
                Ty::Bool
            }
        };
        Ok(IrExpr { ty, kind: IrExprKind::Binary { op, lhs: Box::new(l), rhs: Box::new(r) } })
    }

    fn positional_args(
        &mut self,
        args: &[ast::CallArg],
        line: u32,
        what: &str,
    ) -> Result<Vec<IrExpr>, TypeError> {
        let mut out = Vec::new();
        for a in args {
            if a.name.is_some() {
                return error(line, 1, format!("{what} does not take named arguments"));
            }
            out.push(self.check_expr(&a.value)?);
        }
        Ok(out)
    }

    fn check_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        match &callee.kind {
            ast::ExprKind::Var(name) => self.check_named_call(name, args, line, col),
            ast::ExprKind::Field { recv, name } => {
                if let ast::ExprKind::Var(m) = &recv.kind {
                    // Qualified enum variant construction: `Enum.Variant(args)`.
                    if self.lookup(m).is_none() && self.ctx.enums.contains_key(m) {
                        if !self.ctx.enums[m].iter().any(|v| &v.name == name) {
                            return error(line, col, format!("enum {m} has no variant '{name}'"));
                        }
                        return self.build_variant(m, name, args, line, col);
                    }
                    if self.lookup(m).is_none() && self.ctx.deps.contains_key(m) {
                        return self.check_module_call(m, name, args, line, col);
                    }
                }
                self.check_method(recv, name, args, line, col)
            }
            _ => error(line, col, "this expression is not callable"),
        }
    }

    /// Construct `Enum.Variant(args)` (or 0-arg `Enum.Variant`, args
    /// already empty) once the enum name and variant name are both known —
    /// shared by qualified (F12) and unqualified variant construction.
    fn build_variant(
        &mut self,
        ename: &str,
        vname: &str,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        let fields: Vec<Ty> = self.ctx.enums[ename]
            .iter()
            .find(|v| v.name == vname)
            .unwrap()
            .fields
            .iter()
            .map(|(_, t)| t.clone())
            .collect();
        let irs = self.positional_args(args, line, "a variant constructor")?;
        if irs.len() != fields.len() {
            return error(line, col, format!(
                "variant {vname} has {} field(s), got {} argument(s)", fields.len(), irs.len()
            ));
        }
        for (ir, fty) in irs.iter().zip(&fields) {
            self.unify(&ir.ty, fty, line, col)?;
        }
        Ok(IrExpr {
            ty: Ty::Enum(ename.to_string()),
            kind: IrExprKind::NewVariant {
                enum_name: ename.to_string(),
                variant: vname.to_string(),
                args: irs,
            },
        })
    }

    /// `module.func(args)` — concrete or generic cross-module call.
    fn check_module_call(
        &mut self,
        module: &str,
        fname: &str,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        let dep = self.ctx.deps[module].clone();
        if let Some(sig) = dep.funcs.get(fname) {
            if !crate::sig_portable(sig) {
                return error(
                    line,
                    col,
                    format!("'{module}.{fname}' uses module-local types in its signature and cannot be called across modules yet"),
                );
            }
            let irs = self.check_call_args(fname, &sig.params, args, line, col)?;
            return Ok(IrExpr {
                ty: sig.ret.clone().unwrap_or_else(void),
                kind: IrExprKind::CallFunc { name: format!("{module}.{fname}"), args: irs },
            });
        }
        if let Some(template) = dep.generics.get(fname) {
            return self.check_generic_call_in(
                Some(module),
                fname,
                template,
                &dep.inst,
                args,
                line,
                col,
            );
        }
        error(line, col, format!("module '{module}' has no function '{fname}'"))
    }

    fn check_named_call(
        &mut self,
        name: &str,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        // Function-typed local (parameters holding function references).
        if let Some(local) = self.lookup(name) {
            let lty = local.ty.clone();
            match self.shallow(&lty) {
                Ty::Func { params, ret } => {
                    let irs = self.positional_args(args, line, "a function value")?;
                    if irs.len() != params.len() {
                        return error(
                            line,
                            col,
                            format!("function value '{name}' expects {} argument(s), got {}", params.len(), irs.len()),
                        );
                    }
                    for (ir, p) in irs.iter().zip(&params) {
                        self.unify(&ir.ty, p, line, col)?;
                    }
                    let callee_ir =
                        IrExpr { ty: lty, kind: IrExprKind::Local(name.to_string()) };
                    return Ok(IrExpr {
                        ty: ret.map(|b| *b).unwrap_or_else(void),
                        kind: IrExprKind::CallValue { callee: Box::new(callee_ir), args: irs },
                    });
                }
                other => {
                    return error(line, col, format!("'{name}' is {} and not callable", self.resolved(&other)))
                }
            }
        }

        // User-defined function.
        if let Some(sig) = self.ctx.funcs.get(name).cloned() {
            let irs = self.check_call_args(name, &sig.params, args, line, col)?;
            return Ok(IrExpr {
                ty: sig.ret.clone().unwrap_or_else(void),
                kind: IrExprKind::CallFunc { name: name.to_string(), args: irs },
            });
        }

        // Generic function: infer type arguments, request an instantiation.
        if let Some(template) = self.ctx.generics.get(name).cloned() {
            return self.check_generic_call(name, &template, args, line, col);
        }

        // Option/Result constructors.
        match name {
            "Some" | "Ok" | "Err" => {
                let irs = self.positional_args(args, line, name)?;
                if irs.len() != 1 {
                    return error(line, col, format!("{name} takes exactly one argument"));
                }
                let inner = irs[0].ty.clone();
                let (ty, ename) = match name {
                    "Some" => (Ty::Option_(Box::new(inner)), "Option"),
                    "Ok" => (Ty::Result_(Box::new(inner), Box::new(self.fresh())), "Result"),
                    _ => (Ty::Result_(Box::new(self.fresh()), Box::new(inner)), "Result"),
                };
                return Ok(IrExpr {
                    ty,
                    kind: IrExprKind::NewVariant {
                        enum_name: ename.into(),
                        variant: name.into(),
                        args: irs,
                    },
                });
            }
            _ => {}
        }

        // Enum variant constructor.
        if let Some(enums) = self.ctx.variants.get(name).cloned() {
            if enums.len() > 1 {
                return error(
                    line,
                    col,
                    format!("variant '{name}' is ambiguous ({}); qualify it", enums.join(", ")),
                );
            }
            let ename = enums[0].clone();
            let fields: Vec<Ty> = self.ctx.enums[&ename]
                .iter()
                .find(|v| v.name == name)
                .unwrap()
                .fields
                .iter()
                .map(|(_, t)| t.clone())
                .collect();
            let irs = self.positional_args(args, line, "a variant constructor")?;
            if irs.len() != fields.len() {
                return error(
                    line,
                    col,
                    format!("variant {name} has {} field(s), got {} argument(s)", fields.len(), irs.len()),
                );
            }
            for (ir, fty) in irs.iter().zip(&fields) {
                self.unify(&ir.ty, fty, line, col)?;
            }
            return Ok(IrExpr {
                ty: Ty::Enum(ename.clone()),
                kind: IrExprKind::NewVariant { enum_name: ename, variant: name.into(), args: irs },
            });
        }

        // Record construction (positional or fully named).
        if let Some(fields) = self.ctx.records.get(name).cloned() {
            let all_named = args.iter().all(|a| a.name.is_some());
            let all_positional = args.iter().all(|a| a.name.is_none());
            if !(all_named || all_positional) {
                return error(line, col, "record construction is either all positional or all named");
            }
            let mut irs: Vec<IrExpr> = Vec::new();
            if all_positional {
                if args.len() != fields.len() {
                    return error(
                        line,
                        col,
                        format!("record {name} has {} field(s), got {} argument(s)", fields.len(), args.len()),
                    );
                }
                for (a, (_, fty)) in args.iter().zip(&fields) {
                    let ir = self.check_expr(&a.value)?;
                    self.unify(&ir.ty, fty, a.value.line, a.value.col)?;
                    irs.push(ir);
                }
            } else {
                if args.len() != fields.len() {
                    return error(
                        line,
                        col,
                        format!("record {name} construction must name every field exactly once"),
                    );
                }
                for (fname, fty) in &fields {
                    let Some(a) = args.iter().find(|a| a.name.as_deref() == Some(fname)) else {
                        return error(line, col, format!("record {name} construction is missing field '{fname}'"));
                    };
                    let ir = self.check_expr(&a.value)?;
                    self.unify(&ir.ty, fty, a.value.line, a.value.col)?;
                    irs.push(ir);
                }
            }
            return Ok(IrExpr {
                ty: Ty::Record(name.to_string()),
                kind: IrExprKind::NewRecord { name: name.to_string(), args: irs },
            });
        }

        // Builtin free functions.
        self.check_builtin_free(name, args, line, col)
    }

    /// Shared argument checking for concrete and instantiated signatures:
    /// arity, inout place rules, no named args, unification.
    fn check_call_args(
        &mut self,
        name: &str,
        params: &[(Ty, bool)],
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<Vec<IrExpr>, TypeError> {
        if args.len() != params.len() {
            return error(
                line,
                col,
                format!("function '{name}' expects {} argument(s), got {}", params.len(), args.len()),
            );
        }
        let mut irs = Vec::new();
        let mut inout_roots: Vec<String> = Vec::new();
        for (a, (pty, inout)) in args.iter().zip(params) {
            if a.name.is_some() {
                return error(line, col, "functions do not take named arguments");
            }
            if *inout {
                let root = inout_root(&a.value).ok_or_else(|| TypeError {
                    line,
                    col,
                    msg: "inout arguments must be a plain variable or record-field path".into(),
                })?;
                if inout_roots.contains(&root) {
                    return error(
                        line,
                        col,
                        format!("variable '{root}' is passed to two inout parameters of one call"),
                    );
                }
                inout_roots.push(root);
                // Must be a mutable place; reuse the place checker.
                let (_, pty_actual) = self.check_place(&a.value)?;
                self.unify(&pty_actual, pty, line, col)?;
                irs.push(self.check_expr(&a.value)?);
            } else {
                let ir = self.check_expr(&a.value)?;
                self.unify(&ir.ty, pty, a.value.line, a.value.col)?;
                irs.push(ir);
            }
        }
        Ok(irs)
    }

    /// Deeply resolved type with no inference variables left, or None.
    fn resolve_concrete(&self, ty: &Ty) -> Option<Ty> {
        let t = self.resolved(ty);
        fn no_infer(t: &Ty) -> bool {
            match t {
                Ty::Infer(_) => false,
                Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => no_infer(e),
                Ty::Map(k, v) => no_infer(k) && no_infer(v),
                Ty::Result_(a, b) => no_infer(a) && no_infer(b),
                Ty::Tuple(ts) => ts.iter().all(no_infer),
                Ty::Func { params, ret } => {
                    params.iter().all(no_infer)
                        && ret.as_ref().map(|r| no_infer(r)).unwrap_or(true)
                }
                _ => true,
            }
        }
        no_infer(&t).then_some(t)
    }

    fn check_generic_call(
        &mut self,
        name: &str,
        template: &ast::FuncDecl,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        let inst = std::rc::Rc::clone(&self.ctx.inst);
        self.check_generic_call_in(None, name, template, &inst, args, line, col)
    }

    /// Generic call against a template owned by `module` (None = this one);
    /// the instantiation request lands in the owner's queue.
    #[allow(clippy::too_many_arguments)]
    fn check_generic_call_in(
        &mut self,
        module: Option<&str>,
        name: &str,
        template: &ast::FuncDecl,
        target_inst: &std::rc::Rc<std::cell::RefCell<crate::InstState>>,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        // Generic parameters become fresh inference variables; unifying the
        // arguments against the parameter types pins them down.
        let mut gmap: HashMap<String, Ty> = HashMap::new();
        for g in &template.generics {
            let v = self.fresh();
            gmap.insert(g.clone(), v);
        }
        // A cross-module template must not mention its module's local types;
        // resolving against an empty name table enforces that naturally.
        let type_names = if module.is_none() { self.type_names() } else { HashMap::new() };
        let mut params: Vec<(Ty, bool)> = Vec::new();
        for p in &template.params {
            let ty = crate::resolve_type_with(&p.ty, &type_names, &gmap, line).map_err(|e| {
                cross_module_type_err(module, name, e)
            })?;
            params.push((ty, p.inout));
        }
        let ret = match &template.ret {
            Some(r) => Some(
                crate::resolve_type_with(r, &type_names, &gmap, line)
                    .map_err(|e| cross_module_type_err(module, name, e))?,
            ),
            None => None,
        };
        let irs = self.check_call_args(name, &params, args, line, col)?;

        let mut type_args = Vec::new();
        for g in &template.generics {
            let Some(t) = self.resolve_concrete(&gmap[g]) else {
                return error(
                    line,
                    col,
                    format!("cannot infer type parameter '{g}' for '{name}' — annotate the arguments or result"),
                );
            };
            type_args.push(t);
        }
        let mangled = sudoc_ir::mangle::instantiation_name(name, &type_args);
        {
            let mut inst = target_inst.borrow_mut();
            if !inst.sigs.contains_key(&mangled) {
                let sig = crate::FuncSig {
                    params: params
                        .iter()
                        .map(|(t, io)| {
                            (self.resolve_concrete(t).expect("params concrete after unification"), *io)
                        })
                        .collect(),
                    ret: ret.as_ref().map(|r| {
                        self.resolve_concrete(r).expect("ret concrete after unification")
                    }),
                };
                inst.sigs.insert(mangled.clone(), sig);
                inst.queue.push((name.to_string(), type_args, mangled.clone()));
            }
        }
        let ret_ty = ret
            .as_ref()
            .map(|r| self.resolve_concrete(r).expect("ret concrete"))
            .unwrap_or_else(void);
        let call_name = match module {
            Some(m) => format!("{m}.{mangled}"),
            None => mangled,
        };
        Ok(IrExpr { ty: ret_ty, kind: IrExprKind::CallFunc { name: call_name, args: irs } })
    }

    fn type_names(&self) -> HashMap<String, bool> {
        let mut names = HashMap::new();
        for n in self.ctx.records.keys() {
            names.insert(n.clone(), true);
        }
        for n in self.ctx.enums.keys() {
            names.insert(n.clone(), false);
        }
        names
    }

    fn check_builtin_free(
        &mut self,
        name: &str,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        let irs = self.positional_args(args, line, name)?;
        let expect_arity = |n: usize| -> Result<(), TypeError> {
            if irs.len() == n {
                Ok(())
            } else {
                error(line, col, format!("{name}() expects {n} argument(s), got {}", irs.len()))
            }
        };
        let numeric_head = |ck: &Self, t: &Ty| -> Option<Ty> {
            match ck.shallow(t) {
                Ty::Int => Some(Ty::Int),
                Ty::Float => Some(Ty::Float),
                _ => None,
            }
        };
        match name {
            "abs" => {
                expect_arity(1)?;
                match numeric_head(self, &irs[0].ty) {
                    Some(Ty::Int) => Ok(IrExpr { ty: Ty::Int, kind: IrExprKind::Builtin { builtin: Builtin::AbsInt, args: irs } }),
                    Some(_) => Ok(IrExpr { ty: Ty::Float, kind: IrExprKind::Builtin { builtin: Builtin::AbsFloat, args: irs } }),
                    None => error(line, col, format!("abs() needs int or float, found {}", self.resolved(&irs[0].ty))),
                }
            }
            "min" | "max" => {
                expect_arity(2)?;
                let l = irs[0].ty.clone();
                let r = irs[1].ty.clone();
                self.unify(&l, &r, line, col)?;
                let builtin = match (numeric_head(self, &l), name) {
                    (Some(Ty::Int), "min") => Builtin::MinInt,
                    (Some(Ty::Int), _) => Builtin::MaxInt,
                    (Some(_), "min") => Builtin::MinFloat,
                    (Some(_), _) => Builtin::MaxFloat,
                    (None, _) => {
                        return error(line, col, format!("{name}() needs int or float, found {}", self.resolved(&l)))
                    }
                };
                let ty = self.shallow(&l);
                Ok(IrExpr { ty, kind: IrExprKind::Builtin { builtin, args: irs } })
            }
            "float" => {
                expect_arity(1)?;
                self.unify(&irs[0].ty.clone(), &Ty::Int, line, col)?;
                Ok(IrExpr { ty: Ty::Float, kind: IrExprKind::Builtin { builtin: Builtin::FloatOfInt, args: irs } })
            }
            "int" => {
                expect_arity(1)?;
                self.unify(&irs[0].ty.clone(), &Ty::Float, line, col)?;
                Ok(IrExpr { ty: Ty::Int, kind: IrExprKind::Builtin { builtin: Builtin::IntOfFloat, args: irs } })
            }
            "floor" | "ceil" | "round" | "sqrt" => {
                expect_arity(1)?;
                self.unify(&irs[0].ty.clone(), &Ty::Float, line, col)?;
                let builtin = match name {
                    "floor" => Builtin::Floor,
                    "ceil" => Builtin::Ceil,
                    "round" => Builtin::Round,
                    _ => Builtin::Sqrt,
                };
                Ok(IrExpr { ty: Ty::Float, kind: IrExprKind::Builtin { builtin, args: irs } })
            }
            "filled" => {
                expect_arity(2)?;
                self.unify(&irs[0].ty.clone(), &Ty::Int, line, col)?;
                let elem = irs[1].ty.clone();
                Ok(IrExpr { ty: Ty::List(Box::new(elem)), kind: IrExprKind::Builtin { builtin: Builtin::Filled, args: irs } })
            }
            "Map" => {
                expect_arity(0)?;
                let (k, v) = (self.fresh(), self.fresh());
                Ok(IrExpr { ty: Ty::Map(Box::new(k), Box::new(v)), kind: IrExprKind::Builtin { builtin: Builtin::NewMap, args: irs } })
            }
            "Set" => {
                expect_arity(0)?;
                let t = self.fresh();
                Ok(IrExpr { ty: Ty::Set(Box::new(t)), kind: IrExprKind::Builtin { builtin: Builtin::NewSet, args: irs } })
            }
            _ => error(line, col, format!("unknown function '{name}'")),
        }
    }

    fn check_method(
        &mut self,
        recv: &ast::Expr,
        name: &str,
        args: &[ast::CallArg],
        line: u32,
        col: u32,
    ) -> Result<IrExpr, TypeError> {
        // Peek the receiver type without committing to expression lowering:
        // mutating methods need the receiver as a Place instead.
        let recv_ir = self.check_expr(recv)?;
        let recv_ty = self.shallow(&recv_ir.ty);

        struct M {
            builtin: Builtin,
            arg_tys: Vec<Ty>,
            ret: Ty, // void() for none
        }
        let m: M = match (&recv_ty, name) {
            (Ty::List(e), "append") => M { builtin: Builtin::ListAppend, arg_tys: vec![(**e).clone()], ret: void() },
            (Ty::List(e), "pop") => M { builtin: Builtin::ListPop, arg_tys: vec![], ret: (**e).clone() },
            (Ty::List(e), "insert") => M { builtin: Builtin::ListInsert, arg_tys: vec![Ty::Int, (**e).clone()], ret: void() },
            (Ty::List(e), "remove_at") => M { builtin: Builtin::ListRemoveAt, arg_tys: vec![Ty::Int], ret: (**e).clone() },
            (Ty::List(_), "swap") => M { builtin: Builtin::ListSwap, arg_tys: vec![Ty::Int, Ty::Int], ret: void() },
            (Ty::List(_), "sort") => M { builtin: Builtin::ListSort, arg_tys: vec![], ret: void() },
            (Ty::Map(k, v), "get") => M { builtin: Builtin::MapGet, arg_tys: vec![(**k).clone()], ret: Ty::Option_(v.clone()) },
            (Ty::Map(k, _), "has") => M { builtin: Builtin::MapHas, arg_tys: vec![(**k).clone()], ret: Ty::Bool },
            (Ty::Map(k, _), "delete") => M { builtin: Builtin::MapDelete, arg_tys: vec![(**k).clone()], ret: Ty::Bool },
            (Ty::Map(k, _), "keys") => M { builtin: Builtin::MapKeys, arg_tys: vec![], ret: Ty::List(k.clone()) },
            (Ty::Map(_, v), "values") => M { builtin: Builtin::MapValues, arg_tys: vec![], ret: Ty::List(v.clone()) },
            (Ty::Set(t), "add") => M { builtin: Builtin::SetAdd, arg_tys: vec![(**t).clone()], ret: Ty::Bool },
            (Ty::Set(t), "has") => M { builtin: Builtin::SetHas, arg_tys: vec![(**t).clone()], ret: Ty::Bool },
            (Ty::Set(t), "remove") => M { builtin: Builtin::SetRemove, arg_tys: vec![(**t).clone()], ret: Ty::Bool },
            (Ty::Set(t), "items") => M { builtin: Builtin::SetItems, arg_tys: vec![], ret: Ty::List(t.clone()) },
            (Ty::Option_(_), "is_some") => M { builtin: Builtin::OptIsSome, arg_tys: vec![], ret: Ty::Bool },
            (Ty::Option_(_), "is_none") => M { builtin: Builtin::OptIsNone, arg_tys: vec![], ret: Ty::Bool },
            (Ty::Option_(t), "unwrap") => M { builtin: Builtin::OptUnwrap, arg_tys: vec![], ret: (**t).clone() },
            (Ty::Option_(t), "get_or") => M { builtin: Builtin::OptGetOr, arg_tys: vec![(**t).clone()], ret: (**t).clone() },
            (Ty::Result_(..), "is_ok") => M { builtin: Builtin::ResIsOk, arg_tys: vec![], ret: Ty::Bool },
            (Ty::Result_(..), "is_err") => M { builtin: Builtin::ResIsErr, arg_tys: vec![], ret: Ty::Bool },
            (Ty::Result_(t, _), "unwrap") => M { builtin: Builtin::ResUnwrap, arg_tys: vec![], ret: (**t).clone() },
            (Ty::Result_(t, _), "get_or") => M { builtin: Builtin::ResGetOr, arg_tys: vec![(**t).clone()], ret: (**t).clone() },
            (other, _) => {
                return error(
                    line,
                    col,
                    format!("{} has no method '{name}'", self.resolved(other)),
                )
            }
        };

        let irs = self.positional_args(args, line, "a method")?;
        if irs.len() != m.arg_tys.len() {
            return error(
                line,
                col,
                format!("{name}() expects {} argument(s), got {}", m.arg_tys.len(), irs.len()),
            );
        }
        for (ir, at) in irs.iter().zip(&m.arg_tys) {
            self.unify(&ir.ty, at, line, col)?;
        }

        if m.builtin.mutates() {
            let (place, _) = self.check_place(recv).map_err(|e| TypeError {
                line: e.line,
                col: e.col,
                msg: format!("'{name}' mutates its receiver, which must be a mutable variable ({})", e.msg),
            })?;
            Ok(IrExpr {
                ty: m.ret,
                kind: IrExprKind::MutBuiltin {
                    builtin: m.builtin,
                    recv: place,
                    recv_ty: recv_ty.clone(),
                    args: irs,
                },
            })
        } else {
            let mut all = vec![recv_ir];
            all.extend(irs);
            Ok(IrExpr { ty: m.ret, kind: IrExprKind::Builtin { builtin: m.builtin, args: all } })
        }
    }
}

fn cross_module_type_err(module: Option<&str>, name: &str, e: TypeError) -> TypeError {
    match module {
        None => e,
        Some(m) => TypeError {
            line: e.line,
            col: e.col,
            msg: format!(
                "'{m}.{name}' uses module-local types in its signature and cannot be called across modules yet ({})",
                e.msg
            ),
        },
    }
}

/// Root variable of an inout argument path (plain var or record-field chain).
fn inout_root(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Var(n) => Some(n.clone()),
        ast::ExprKind::Field { recv, .. } => inout_root(recv),
        _ => None,
    }
}

fn contains_func(ty: &Ty) -> bool {
    match ty {
        Ty::Func { .. } => true,
        Ty::List(t) | Ty::Set(t) | Ty::Option_(t) => contains_func(t),
        Ty::Map(k, v) => contains_func(k) || contains_func(v),
        Ty::Result_(a, b) => contains_func(a) || contains_func(b),
        Ty::Tuple(ts) => ts.iter().any(contains_func),
        _ => false,
    }
}

/// "type mismatch: int vs float" -> "int vs float" for nicer nested messages.
fn trim_mismatch(msg: &str) -> &str {
    msg.strip_prefix("type mismatch: ").unwrap_or(msg)
}
