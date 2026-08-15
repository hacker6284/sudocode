//! Multi-module programs in Swift: a single translation unit.
//!
//! Dependency modules' **value** symbols (functions, constants) are
//! qualified via `sudoc_ir::mangle::qualify_value`. Record and enum
//! IR symbols are already unique; this pass does not rename types.
//! Qualified references (`sorting.quicksort`) go
//! through the same value scheme. The entry module keeps bare names.
//! Tuple-struct names are shape-keyed and shared across the merged
//! program — not module-prefixed.

use std::collections::HashSet;

use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrStmt, Place};

/// Merge a program into one renamed module (deps first, entry last).
pub(crate) fn merge(modules: &[IrModule]) -> IrModule {
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
    merged.records = sudoc_ir::topo_sort_records(merged.records);
    merged
}

/// Qualify local symbols via `sudoc_ir::mangle` (None = entry: only rewrite
/// qualified references).
fn rename_module(m: &mut IrModule, prefix: Option<&str>) {
    let local_values: HashSet<String> = m
        .funcs
        .iter()
        .map(|f| f.name.clone())
        .chain(m.consts.iter().map(|c| c.name.clone()))
        .collect();
    let r = Renamer {
        prefix: prefix.map(str::to_string),
        local_values,
    };

    for c in &mut m.consts {
        c.name = r.value_name(&c.name);
        r.expr(&mut c.value);
    }
    for f in &mut m.funcs {
        f.name = r.value_name(&f.name);
        r.block(&mut f.body);
    }
    for t in &mut m.tests {
        r.block(&mut t.body);
    }
}

struct Renamer {
    prefix: Option<String>,
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
            Place::Index { base, index, .. } => {
                self.place(base);
                self.expr(index);
            }
            Place::Field { base, .. } => {
                self.place(base);
            }
        }
    }

    fn expr(&self, e: &mut IrExpr) {
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
            IrExprKind::NewRecord { args, .. } => {
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::NewVariant { args, .. } => {
                args.iter_mut().for_each(|a| self.expr(a));
            }
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) | IrExprKind::Builtin { args: xs, .. } => {
                xs.iter_mut().for_each(|x| self.expr(x));
            }
            IrExprKind::MutBuiltin { recv, args, .. } => {
                self.place(recv);
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
