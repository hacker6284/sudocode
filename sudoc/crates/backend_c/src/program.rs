//! Multi-module programs in C (lockstep.md §8): a single translation
//! unit. Dependency modules' symbols (functions, constants, records,
//! enums) are qualified via the shared, collision-proof
//! `sudoc_ir::mangle` scheme (`sudo_M<len><module>_<len><name>` /
//! `Sudo_M<len><module>_<len><name>`) for provably collision-free
//! merging; qualified references (`sorting.quicksort`) go through the
//! same scheme. The entry module keeps bare names.

use std::collections::HashSet;

use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrPattern, IrStmt, Place, Ty};

pub fn emit_program(modules: &[IrModule], with_tests: bool) -> String {
    crate::emit(&merge(modules), with_tests)
}

/// Merge a program into one renamed module (also used for header emission).
pub fn merge(modules: &[IrModule]) -> IrModule {
    let (entry, deps) = modules.split_last().expect("at least the entry module");
    let mut merged = IrModule {
        name: entry.name.clone(),
        imports: Vec::new(),
        records: Vec::new(),
        enums: Vec::new(),
        consts: Vec::new(),
        funcs: Vec::new(),
        tests: Vec::new(),
    };
    for dep in deps {
        let mut m = dep.clone();
        // Only the entry module's exports get host wrappers in the merged TU.
        for f in &mut m.funcs {
            f.export = false;
        }
        rename_module(&mut m, Some(&dep.name));
        merged.records.extend(m.records);
        merged.enums.extend(m.enums);
        merged.consts.extend(m.consts);
        merged.funcs.extend(m.funcs);
        // Dependency tests are not part of the entry's test run.
    }
    let mut e = entry.clone();
    rename_module(&mut e, None);
    merged.records.extend(e.records);
    merged.enums.extend(e.enums);
    merged.consts.extend(e.consts);
    merged.funcs.extend(e.funcs);
    merged.tests = e.tests;
    crate::reserved::rename_reserved(&merged)
}

/// Qualify local symbols via `sudoc_ir::mangle` (None = entry: only rewrite
/// qualified references).
fn rename_module(m: &mut IrModule, prefix: Option<&str>) {
    let local_types: HashSet<String> = m
        .records
        .iter()
        .map(|r| r.name.clone())
        .chain(m.enums.iter().map(|e| e.name.clone()))
        .collect();
    let local_values: HashSet<String> = m
        .funcs
        .iter()
        .map(|f| f.name.clone())
        .chain(m.consts.iter().map(|c| c.name.clone()))
        .collect();
    let r = Renamer { prefix: prefix.map(str::to_string), local_types, local_values };

    for rec in &mut m.records {
        rec.name = r.type_name(&rec.name);
        for (_, t) in &mut rec.fields {
            r.ty(t);
        }
    }
    for en in &mut m.enums {
        en.name = r.type_name(&en.name);
        for v in &mut en.variants {
            for (_, t) in &mut v.fields {
                r.ty(t);
            }
        }
    }
    for c in &mut m.consts {
        c.name = r.value_name(&c.name);
        r.ty(&mut c.ty);
        r.expr(&mut c.value);
    }
    for f in &mut m.funcs {
        f.name = r.value_name(&f.name);
        for p in &mut f.params {
            r.ty(&mut p.ty);
        }
        if let Some(t) = &mut f.ret {
            r.ty(t);
        }
        r.block(&mut f.body);
    }
    for t in &mut m.tests {
        r.block(&mut t.body);
    }
}

struct Renamer {
    prefix: Option<String>,
    local_types: HashSet<String>,
    local_values: HashSet<String>,
}

impl Renamer {
    /// A referenced value name: local -> qualified; `a.b` -> mangle qualify.
    fn value_ref(&self, name: &str) -> String {
        if let Some((m, f)) = name.split_once('.') {
            return sudoc_ir::mangle::qualify_value(Some(m), f);
        }
        if self.local_values.contains(name) {
            return self.value_name(name);
        }
        name.to_string()
    }

    fn value_name(&self, name: &str) -> String {
        sudoc_ir::mangle::qualify_value(self.prefix.as_deref(), name)
    }

    fn type_name(&self, name: &str) -> String {
        sudoc_ir::mangle::qualify_type(self.prefix.as_deref(), name)
    }

    fn type_ref(&self, name: &str) -> String {
        if self.local_types.contains(name) {
            self.type_name(name)
        } else {
            name.to_string()
        }
    }

    fn ty(&self, t: &mut Ty) {
        match t {
            Ty::Record(n) | Ty::Enum(n) => *n = self.type_ref(n),
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => self.ty(e),
            Ty::Map(k, v) => {
                self.ty(k);
                self.ty(v);
            }
            Ty::Result_(a, b) => {
                self.ty(a);
                self.ty(b);
            }
            Ty::Tuple(ts) => ts.iter_mut().for_each(|t| self.ty(t)),
            Ty::Func { params, ret } => {
                params.iter_mut().for_each(|t| self.ty(t));
                if let Some(r) = ret {
                    self.ty(r);
                }
            }
            _ => {}
        }
    }

    fn block(&self, stmts: &mut [IrStmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&self, s: &mut IrStmt) {
        match s {
            IrStmt::Assign { target, value, .. } => {
                self.place(target);
                self.expr(value);
            }
            IrStmt::TupleAssign { value, .. } => self.expr(value),
            IrStmt::Expr(e) => self.expr(e),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    self.expr(c);
                    self.block(b);
                }
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            IrStmt::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                self.expr(from);
                self.expr(to);
                self.block(body);
            }
            IrStmt::ForIn { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            IrStmt::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for a in arms {
                    if let IrPattern::Variant { enum_name, .. } = &mut a.pattern {
                        if enum_name != "Option" && enum_name != "Result" {
                            *enum_name = self.type_ref(enum_name);
                        }
                    }
                    self.block(&mut a.body);
                }
            }
            IrStmt::Return(Some(e)) => self.expr(e),
            IrStmt::Assert { cond, .. } => self.expr(cond),
            IrStmt::ExpectTrap { body, .. } => self.block(body),
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }

    fn place(&self, p: &mut Place) {
        match p {
            Place::Var(_) => {}
            Place::Index { base, base_ty, index } => {
                self.place(base);
                self.ty(base_ty);
                self.expr(index);
            }
            Place::Field { base, base_ty, .. } => {
                self.place(base);
                self.ty(base_ty);
            }
        }
    }

    fn expr(&self, e: &mut IrExpr) {
        self.ty(&mut e.ty);
        match &mut e.kind {
            IrExprKind::Const(n) | IrExprKind::FuncRef(n) => *n = self.value_ref(n),
            IrExprKind::CallFunc { name, args } => {
                *name = self.value_ref(name);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::CallValue { callee, args } => {
                self.expr(callee);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::NewRecord { name, args } => {
                *name = self.type_ref(name);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::NewVariant { enum_name, args, .. } => {
                if enum_name != "Option" && enum_name != "Result" {
                    *enum_name = self.type_ref(enum_name);
                }
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) | IrExprKind::Builtin { args: xs, .. } => {
                xs.iter_mut().for_each(|x| self.expr(x));
            }
            IrExprKind::MutBuiltin { recv, recv_ty, args, .. } => {
                self.place(recv);
                self.ty(recv_ty);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::GetField { recv, .. } => self.expr(recv),
            IrExprKind::Index { recv, index } => {
                self.expr(recv);
                self.expr(index);
            }
            IrExprKind::Unary { operand, .. } => self.expr(operand),
            IrExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            _ => {}
        }
    }
}
