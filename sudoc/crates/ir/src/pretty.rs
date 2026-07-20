//! Stable, human-reviewable dump of the typed IR, for golden-file tests.
//! The format is line-oriented and deterministic; goldens live in
//! conformance/golden/*.ir and are reviewed like code.

use crate::*;
use std::fmt::Write;

pub fn dump(m: &IrModule) -> String {
    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(w, "module {}", m.name);
    for r in &m.records {
        let fields: Vec<String> = r.fields.iter().map(|(n, t)| format!("{n}: {t}")).collect();
        let _ = writeln!(w, "record {} ({})", r.name, fields.join(", "));
    }
    for e in &m.enums {
        let _ = writeln!(w, "enum {}", e.name);
        for v in &e.variants {
            let fields: Vec<String> = v.fields.iter().map(|(n, t)| format!("{n}: {t}")).collect();
            let _ = writeln!(w, "  {}({})", v.name, fields.join(", "));
        }
    }
    for c in &m.consts {
        let _ = writeln!(w, "const {}: {} = {}", c.name, c.ty, expr(&c.value));
    }
    for f in &m.funcs {
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                format!("{}: {}{}", p.name, if p.inout { "inout " } else { "" }, p.ty)
            })
            .collect();
        let ret = match &f.ret {
            Some(t) => format!(" -> {t}"),
            None => String::new(),
        };
        let exp = if f.export { "export " } else { "" };
        let _ = writeln!(w, "{}func {}({}){}", exp, f.name, params.join(", "), ret);
        block(w, &f.body, 1);
    }
    for t in &m.tests {
        let _ = writeln!(w, "test {:?}", t.name);
        block(w, &t.body, 1);
    }
    out
}

fn block(w: &mut String, stmts: &[IrStmt], depth: usize) {
    for s in stmts {
        stmt(w, s, depth);
    }
}

fn stmt(w: &mut String, s: &IrStmt, depth: usize) {
    let pad = "  ".repeat(depth);
    match s {
        IrStmt::Assign { target, value, declares } => {
            let d = if *declares { "let " } else { "" };
            let _ = writeln!(w, "{pad}{d}{} = {}", place(target), expr(value));
        }
        IrStmt::TupleAssign { targets, declares, value } => {
            let ts: Vec<String> = targets
                .iter()
                .zip(declares)
                .map(|(t, d)| format!("{}{}", if *d { "let " } else { "" }, t))
                .collect();
            let _ = writeln!(w, "{pad}({}) = {}", ts.join(", "), expr(value));
        }
        IrStmt::Expr(e) => {
            let _ = writeln!(w, "{pad}{}", expr(e));
        }
        IrStmt::If { arms, else_block } => {
            for (i, (c, b)) in arms.iter().enumerate() {
                let kw = if i == 0 { "if" } else { "else if" };
                let _ = writeln!(w, "{pad}{kw} {}", expr(c));
                block(w, b, depth + 1);
            }
            if let Some(b) = else_block {
                let _ = writeln!(w, "{pad}else");
                block(w, b, depth + 1);
            }
        }
        IrStmt::While { cond, body } => {
            let _ = writeln!(w, "{pad}while {}", expr(cond));
            block(w, body, depth + 1);
        }
        IrStmt::ForRange { var, from, to, down, body } => {
            let kw = if *down { "downto" } else { "to" };
            let _ = writeln!(w, "{pad}for {var} = {} {kw} {}", expr(from), expr(to));
            block(w, body, depth + 1);
        }
        IrStmt::ForIn { vars, iter, body } => {
            let _ = writeln!(w, "{pad}for {} in {}", vars.join(", "), expr(iter));
            block(w, body, depth + 1);
        }
        IrStmt::Match { scrutinee, arms } => {
            let _ = writeln!(w, "{pad}match {}", expr(scrutinee));
            for arm in arms {
                let p = match &arm.pattern {
                    IrPattern::Int(v) => format!("{v}"),
                    IrPattern::Bool(v) => format!("{v}"),
                    IrPattern::Wildcard => "_".into(),
                    IrPattern::Variant { enum_name, variant, binders } => {
                        if binders.is_empty() {
                            format!("{enum_name}.{variant}")
                        } else {
                            format!("{enum_name}.{variant}({})", binders.join(", "))
                        }
                    }
                };
                let _ = writeln!(w, "{pad}  case {p}");
                block(w, &arm.body, depth + 2);
            }
        }
        IrStmt::Return(v) => match v {
            Some(e) => {
                let _ = writeln!(w, "{pad}return {}", expr(e));
            }
            None => {
                let _ = writeln!(w, "{pad}return");
            }
        },
        IrStmt::Assert { cond, line } => {
            let _ = writeln!(w, "{pad}assert@{line} {}", expr(cond));
        }
        IrStmt::Skip => {
            let _ = writeln!(w, "{pad}skip");
        }
        IrStmt::Break => {
            let _ = writeln!(w, "{pad}break");
        }
        IrStmt::Continue => {
            let _ = writeln!(w, "{pad}continue");
        }
        IrStmt::ExpectTrap { kind, body, line } => {
            let _ = writeln!(w, "{pad}expect_trap@{line} {kind}");
            block(w, body, depth + 1);
        }
    }
}

fn place(p: &Place) -> String {
    match p {
        Place::Var(n) => n.clone(),
        Place::Index { base, index, .. } => format!("{}[{}]", place(base), expr(index)),
        Place::Field { base, name, .. } => format!("{}.{name}", place(base)),
    }
}

pub fn expr(e: &IrExpr) -> String {
    let t = &e.ty;
    match &e.kind {
        IrExprKind::Int(v) => format!("{v}"),
        IrExprKind::Float(v) => format!("{v:?}"),
        IrExprKind::Bool(v) => format!("{v}"),
        IrExprKind::Text(s) => {
            let rendered: String = s
                .iter()
                .map(|&c| char::from_u32(c as u32).unwrap_or('\u{FFFD}'))
                .collect();
            format!("{rendered:?}")
        }
        IrExprKind::Local(n) => format!("{n}:{t}"),
        IrExprKind::Const(n) => format!("const:{n}"),
        IrExprKind::FuncRef(n) => format!("&{n}"),
        IrExprKind::List(xs) => format!("[{}]:{t}", exprs(xs)),
        IrExprKind::Tuple(xs) => format!("({})", exprs(xs)),
        IrExprKind::CallFunc { name, args } => format!("{name}({})", exprs(args)),
        IrExprKind::CallValue { callee, args } => {
            format!("({})({})", expr(callee), exprs(args))
        }
        IrExprKind::NewRecord { name, args } => format!("{name}{{{}}}", exprs(args)),
        IrExprKind::NewVariant { enum_name, variant, args } => {
            if args.is_empty() {
                format!("{enum_name}.{variant}")
            } else {
                format!("{enum_name}.{variant}({})", exprs(args))
            }
        }
        IrExprKind::Builtin { builtin, args } => {
            format!("{builtin:?}({})", exprs(args))
        }
        IrExprKind::MutBuiltin { builtin, recv, args, .. } => {
            if args.is_empty() {
                format!("{builtin:?}(&{})", place(recv))
            } else {
                format!("{builtin:?}(&{}, {})", place(recv), exprs(args))
            }
        }
        IrExprKind::GetField { recv, name } => format!("{}.{name}", expr(recv)),
        IrExprKind::Index { recv, index } => format!("{}[{}]", expr(recv), expr(index)),
        IrExprKind::Unary { op, operand } => format!("({op:?} {})", expr(operand)),
        IrExprKind::Binary { op, lhs, rhs } => {
            format!("({op:?} {} {})", expr(lhs), expr(rhs))
        }
    }
}

fn exprs(xs: &[IrExpr]) -> String {
    xs.iter().map(expr).collect::<Vec<_>>().join(", ")
}
