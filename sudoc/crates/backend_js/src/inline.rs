//! Which calls the JS backend inlines, and which list parameters a function
//! may update in place.
//!
//! A helper is inlineable when it is a small straight-line function: no
//! loops, no `match`, no jumps, no `inout`, every call it makes is itself
//! inlineable, and it does not reach itself. The cut is a statement count,
//! not a name list. Number lowering applies inside those inlined bodies.
//!
//! A function steals a list parameter when it is a one-to-one rebuild of
//! that list (`out[i] = f(src[i])`) or it forwards the list, by a transfer,
//! to a function that steals it. Callers that still need the old list share
//! it first so the rebuild's `rc == 1` fast path does not see it.

use std::collections::{HashMap, HashSet};

use sudoc_ir::{BinaryOp, Builtin, IrExpr, IrExprKind, IrFunc, IrModule, IrStmt, Place, Ty};

/// Statement budget for a straight-line helper. `turn_cubie` in Scramble is
/// the largest helper the hot loops call; the cut sits above that shape and
/// below functions that carry a loop.
const MAX_STMTS: usize = 48;

#[derive(Clone)]
pub(crate) struct Rebuild {
    pub src: String,
    pub acc: String,
    pub index: String,
    pub body: Vec<IrStmt>,
}

pub(crate) struct Plan {
    /// Small enough to substitute at a call site.
    pub inlineable: HashSet<String>,
    /// Straight-line. Its body uses number stores; a caller that is not
    /// lowered converts an int result back to BigInt.
    pub lowered: HashSet<String>,
    /// Function name → list parameters it may mutate when `rc == 1`.
    pub steals: HashMap<String, HashSet<String>>,
    pub rebuilds: HashMap<String, Rebuild>,
}

pub(crate) fn plan(module: &IrModule) -> Plan {
    // A helper that only calls lowered helpers is lowered too, so a dot
    // product is not converted back to BigInt at each call.
    let mut lowered = HashSet::new();
    loop {
        let mut grew = false;
        for f in &module.funcs {
            if lowered.contains(&f.name) || !straight_line(f) {
                continue;
            }
            if uses_lowered_int(&f.body) || calls_of(f).iter().any(|c| lowered.contains(c)) {
                lowered.insert(f.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut inlineable = HashSet::new();
    loop {
        let mut grew = false;
        for f in &module.funcs {
            if inlineable.contains(&f.name) || !straight_line(f) || stmt_count(&f.body) > MAX_STMTS
            {
                continue;
            }
            if calls_of(f).iter().all(|c| inlineable.contains(c)) {
                inlineable.insert(f.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let mut rebuilds = HashMap::new();
    for f in &module.funcs {
        if let Some(r) = rebuild_of(f) {
            rebuilds.insert(f.name.clone(), r);
        }
    }

    let mut steals: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, reb) in &rebuilds {
        steals
            .entry(name.clone())
            .or_default()
            .insert(reb.src.clone());
    }
    loop {
        let mut grew = false;
        for f in &module.funcs {
            for p in &f.params {
                if !matches!(p.ty, Ty::List(_)) {
                    continue;
                }
                if steals.get(&f.name).is_some_and(|s| s.contains(&p.name)) {
                    continue;
                }
                if forwards_to_stealer(f, &p.name, &steals) {
                    steals
                        .entry(f.name.clone())
                        .or_default()
                        .insert(p.name.clone());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    Plan {
        inlineable,
        lowered,
        steals,
        rebuilds,
    }
}

/// Int arithmetic or an int comparison. Those are the stores and tests
/// that have to accept either a number or a BigInt.
fn uses_lowered_int(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Assign { value, .. } | IrStmt::TupleAssign { value, .. } | IrStmt::Expr(value) => {
            expr_uses_lowered_int(value)
        }
        IrStmt::Return(Some(v)) | IrStmt::Assert { cond: v, .. } => expr_uses_lowered_int(v),
        IrStmt::If { arms, else_block } => {
            arms.iter()
                .any(|(c, b)| expr_uses_lowered_int(c) || uses_lowered_int(b))
                || else_block.as_ref().is_some_and(|b| uses_lowered_int(b))
        }
        _ => false,
    })
}

fn expr_uses_lowered_int(e: &IrExpr) -> bool {
    match &e.kind {
        IrExprKind::Binary { op, lhs, rhs } => {
            let int = matches!(lhs.ty, Ty::Int);
            let hot = int
                && matches!(
                    op,
                    BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul
                        | BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                        | BinaryOp::Eq
                        | BinaryOp::Ne
                );
            hot || expr_uses_lowered_int(lhs) || expr_uses_lowered_int(rhs)
        }
        IrExprKind::Unary {
            op: sudoc_ir::UnaryOp::Neg,
            ..
        } if matches!(e.ty, Ty::Int) => true,
        IrExprKind::Unary { operand, .. } => expr_uses_lowered_int(operand),
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::MutBuiltin { args: xs, .. } => xs.iter().any(expr_uses_lowered_int),
        IrExprKind::CallValue { callee, args } => {
            expr_uses_lowered_int(callee) || args.iter().any(expr_uses_lowered_int)
        }
        IrExprKind::GetField { recv, .. } | IrExprKind::Index { recv, .. } => {
            expr_uses_lowered_int(recv)
        }
        _ => false,
    }
}

fn straight_line(f: &IrFunc) -> bool {
    !f.params.iter().any(|p| p.inout)
        && stmt_count(&f.body) > 0
        && returns_are_tail(&f.body)
        && always_returns(&f.body)
        && !has_banned(&f.body)
}

fn always_returns(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Return(_) => true,
        IrStmt::If { arms, else_block } => {
            else_block.as_ref().is_some_and(|b| always_returns(b))
                && arms.iter().all(|(_, b)| always_returns(b))
        }
        _ => false,
    })
}

fn has_banned(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::While { .. }
        | IrStmt::ForRange { .. }
        | IrStmt::ForIn { .. }
        | IrStmt::Match { .. }
        | IrStmt::Break
        | IrStmt::Continue
        | IrStmt::ExpectTrap { .. } => true,
        IrStmt::If { arms, else_block } => {
            arms.iter().any(|(_, b)| has_banned(b))
                || else_block.as_ref().is_some_and(|b| has_banned(b))
        }
        _ => false,
    })
}

fn returns_are_tail(stmts: &[IrStmt]) -> bool {
    fn block(stmts: &[IrStmt]) -> bool {
        let Some(pos) = stmts.iter().position(|s| matches!(s, IrStmt::Return(_))) else {
            return stmts.iter().all(|s| match s {
                IrStmt::If { arms, else_block } => {
                    arms.iter().all(|(_, b)| block(b))
                        && else_block.as_ref().is_none_or(|b| block(b))
                }
                _ => true,
            });
        };
        pos + 1 == stmts.len()
            && stmts.iter().take(pos).all(|s| match s {
                IrStmt::If { arms, else_block } => {
                    arms.iter().all(|(_, b)| block(b))
                        && else_block.as_ref().is_none_or(|b| block(b))
                }
                IrStmt::Return(_) => false,
                _ => true,
            })
    }
    block(stmts)
}

fn stmt_count(stmts: &[IrStmt]) -> usize {
    stmts
        .iter()
        .map(|s| {
            1 + match s {
                IrStmt::If { arms, else_block } => {
                    arms.iter().map(|(_, b)| stmt_count(b)).sum::<usize>()
                        + else_block.as_ref().map(|b| stmt_count(b)).unwrap_or(0)
                }
                _ => 0,
            }
        })
        .sum()
}

fn calls_of(f: &IrFunc) -> Vec<String> {
    let mut out = Vec::new();
    collect_calls(&f.body, &mut out);
    out
}

fn collect_calls(stmts: &[IrStmt], out: &mut Vec<String>) {
    for s in stmts {
        walk_stmt_exprs(s, &mut |e| {
            if let IrExprKind::CallFunc { name, .. } = &e.kind {
                out.push(name.clone());
            }
        });
        if let IrStmt::If { arms, else_block } = s {
            for (_, b) in arms {
                collect_calls(b, out);
            }
            if let Some(b) = else_block {
                collect_calls(b, out);
            }
        }
    }
}

fn walk_stmt_exprs(s: &IrStmt, f: &mut dyn FnMut(&IrExpr)) {
    match s {
        IrStmt::Assign { value, .. } => walk_expr(value, f),
        IrStmt::TupleAssign { value, .. } => walk_expr(value, f),
        IrStmt::Expr(e) | IrStmt::Return(Some(e)) => walk_expr(e, f),
        IrStmt::If { arms, .. } => {
            for (c, _) in arms {
                walk_expr(c, f);
            }
        }
        IrStmt::Assert { cond, .. } => walk_expr(cond, f),
        _ => {}
    }
}

fn walk_expr(e: &IrExpr, f: &mut dyn FnMut(&IrExpr)) {
    f(e);
    match &e.kind {
        IrExprKind::List(xs) | IrExprKind::Tuple(xs) => xs.iter().for_each(|x| walk_expr(x, f)),
        IrExprKind::CallFunc { args, .. } | IrExprKind::Builtin { args, .. } => {
            args.iter().for_each(|x| walk_expr(x, f));
        }
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, f);
            args.iter().for_each(|x| walk_expr(x, f));
        }
        IrExprKind::NewRecord { args, .. } | IrExprKind::NewVariant { args, .. } => {
            args.iter().for_each(|x| walk_expr(x, f));
        }
        IrExprKind::MutBuiltin { args, .. } => args.iter().for_each(|x| walk_expr(x, f)),
        IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
            walk_expr(recv, f);
        }
        IrExprKind::Index { recv, index } => {
            walk_expr(recv, f);
            walk_expr(index, f);
        }
        IrExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        _ => {}
    }
}

/// `out[i] = f(src[i])` over `i = 0 .. src.length - 1`, then `return out`.
fn rebuild_of(f: &IrFunc) -> Option<Rebuild> {
    let IrStmt::Assign {
        target: Place::Var(acc),
        value,
        declares: true,
    } = f.body.first()?
    else {
        return None;
    };
    let IrExprKind::List(xs) = &value.kind else {
        return None;
    };
    if !xs.is_empty() {
        return None;
    }
    let IrStmt::ForRange {
        var,
        from,
        to,
        down: false,
        body,
    } = f.body.get(1)?
    else {
        return None;
    };
    let IrExprKind::Int(0) = from.kind else {
        return None;
    };
    let src = len_minus_one(to)?;
    if !f
        .params
        .iter()
        .any(|p| p.name == src && matches!(p.ty, Ty::List(_)))
    {
        return None;
    }
    if f.body.len() != 3 {
        return None;
    }
    let IrStmt::Return(Some(ret)) = f.body.last()? else {
        return None;
    };
    let IrExprKind::Local(ret_name) = &ret.kind else {
        return None;
    };
    if ret_name != acc {
        return None;
    }
    if acc_appends(body, acc)? != 1 {
        return None;
    }
    if !only_src_index(body, &src, var) {
        return None;
    }
    Some(Rebuild {
        src,
        acc: acc.clone(),
        index: var.clone(),
        body: body.clone(),
    })
}

fn len_minus_one(e: &IrExpr) -> Option<String> {
    let IrExprKind::Binary {
        op: BinaryOp::Sub,
        lhs,
        rhs,
    } = &e.kind
    else {
        return None;
    };
    let IrExprKind::Int(1) = rhs.kind else {
        return None;
    };
    let IrExprKind::Builtin {
        builtin: Builtin::ListLength,
        args,
    } = &lhs.kind
    else {
        return None;
    };
    let IrExprKind::Local(name) = &args.first()?.kind else {
        return None;
    };
    Some(name.clone())
}

fn append_target(e: &IrExpr) -> Option<&str> {
    let IrExprKind::MutBuiltin {
        builtin: Builtin::ListAppend,
        recv: Place::Var(name),
        ..
    } = &e.kind
    else {
        return None;
    };
    Some(name)
}

/// How many appends to `acc` every path performs. `None` when paths disagree
/// or `acc` is used some other way.
fn acc_appends(stmts: &[IrStmt], acc: &str) -> Option<usize> {
    let mut n = 0;
    for s in stmts {
        match s {
            IrStmt::Expr(e) => {
                if let Some(target) = append_target(e) {
                    if target != acc {
                        return None;
                    }
                    n += 1;
                } else if expr_mentions(e, acc) {
                    return None;
                }
            }
            IrStmt::If { arms, else_block } => {
                if arms.iter().any(|(c, _)| expr_mentions(c, acc)) {
                    return None;
                }
                let mut counts = Vec::new();
                for (_, b) in arms {
                    counts.push(acc_appends(b, acc)?);
                }
                match else_block {
                    Some(b) => counts.push(acc_appends(b, acc)?),
                    None => counts.push(0),
                }
                if counts.iter().any(|c| *c != counts[0]) {
                    return None;
                }
                n += counts[0];
            }
            IrStmt::Assign { value, target, .. } => {
                if place_mentions(target, acc) || expr_mentions(value, acc) {
                    return None;
                }
            }
            IrStmt::TupleAssign { value, targets, .. } => {
                if targets.iter().any(|t| t == acc) || expr_mentions(value, acc) {
                    return None;
                }
            }
            IrStmt::Return(_) | IrStmt::Break | IrStmt::Continue => return None,
            other => {
                if stmt_mentions(other, acc) {
                    return None;
                }
            }
        }
    }
    Some(n)
}

fn only_src_index(stmts: &[IrStmt], src: &str, index: &str) -> bool {
    fn ok_expr(e: &IrExpr, src: &str, index: &str) -> bool {
        match &e.kind {
            IrExprKind::Local(n) => n != src,
            IrExprKind::Index { recv, index: ix } => match (&recv.kind, &ix.kind) {
                (IrExprKind::Local(r), IrExprKind::Local(i)) if r == src && i == index => true,
                _ => {
                    ok_expr(recv, src, index)
                        && ok_expr(ix, src, index)
                        && !expr_mentions(recv, src)
                }
            },
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) => {
                xs.iter().all(|x| ok_expr(x, src, index))
            }
            IrExprKind::CallFunc { args, .. }
            | IrExprKind::Builtin { args, .. }
            | IrExprKind::NewRecord { args, .. }
            | IrExprKind::NewVariant { args, .. }
            | IrExprKind::MutBuiltin { args, .. } => args.iter().all(|x| ok_expr(x, src, index)),
            IrExprKind::CallValue { callee, args } => {
                ok_expr(callee, src, index) && args.iter().all(|x| ok_expr(x, src, index))
            }
            IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
                ok_expr(recv, src, index)
            }
            IrExprKind::Binary { lhs, rhs, .. } => {
                ok_expr(lhs, src, index) && ok_expr(rhs, src, index)
            }
            _ => true,
        }
    }
    fn ok_stmt(s: &IrStmt, src: &str, index: &str) -> bool {
        match s {
            IrStmt::Assign { value, .. } => ok_expr(value, src, index),
            IrStmt::TupleAssign { value, .. } => ok_expr(value, src, index),
            IrStmt::Expr(e) | IrStmt::Return(Some(e)) | IrStmt::Assert { cond: e, .. } => {
                ok_expr(e, src, index)
            }
            IrStmt::If { arms, else_block } => {
                arms.iter().all(|(c, b)| {
                    ok_expr(c, src, index) && b.iter().all(|s| ok_stmt(s, src, index))
                }) && else_block
                    .as_ref()
                    .is_none_or(|b| b.iter().all(|s| ok_stmt(s, src, index)))
            }
            _ => true,
        }
    }
    stmts.iter().all(|s| ok_stmt(s, src, index))
}

fn expr_mentions(e: &IrExpr, name: &str) -> bool {
    let mut hit = false;
    walk_expr(e, &mut |x| {
        if matches!(&x.kind, IrExprKind::Local(n) if n == name) {
            hit = true;
        }
    });
    hit
}

fn place_mentions(p: &Place, name: &str) -> bool {
    match p {
        Place::Var(n) => n == name,
        Place::Index { base, .. } | Place::Field { base, .. } => place_mentions(base, name),
    }
}

fn stmt_mentions(s: &IrStmt, name: &str) -> bool {
    let mut hit = false;
    walk_stmt_exprs(s, &mut |e| {
        if expr_mentions(e, name) {
            hit = true;
        }
    });
    hit
}

/// `param` is handed to a stealing callee by transfer, possibly through
/// `alias = param` when that assignment is the parameter's last direct use.
fn forwards_to_stealer(f: &IrFunc, param: &str, steals: &HashMap<String, HashSet<String>>) -> bool {
    let mut aliases = HashSet::new();
    aliases.insert(param.to_string());
    let mut stole = false;
    if !forward_stmts(&f.body, param, &mut aliases, steals, &mut stole) {
        return false;
    }
    stole
}

/// `false` when the body mutates an alias other than by a stealing transfer.
fn forward_stmts(
    stmts: &[IrStmt],
    param: &str,
    aliases: &mut HashSet<String>,
    steals: &HashMap<String, HashSet<String>>,
    stole: &mut bool,
) -> bool {
    for s in stmts {
        match s {
            IrStmt::Assign {
                target: Place::Var(dest),
                value,
                ..
            } => {
                if let IrExprKind::Local(src) = &value.kind {
                    if aliases.contains(src) && src == param && !mentions_later(stmts, param) {
                        aliases.insert(dest.clone());
                    }
                }
                if let IrExprKind::CallFunc { name, args } = &value.kind {
                    if transfer_arg(args, aliases, steals, name) {
                        *stole = true;
                    } else if args
                        .iter()
                        .any(|a| matches!(&a.kind, IrExprKind::Local(n) if aliases.contains(n)))
                        && steals.get(name).is_some()
                    {
                        // Passed, but not as a transfer: the call site shares.
                        // The parameter itself is not stolen by this function.
                    }
                }
                if place_is_alias(s, aliases) {
                    return false;
                }
            }
            IrStmt::Return(Some(e)) => {
                if let IrExprKind::CallFunc { name, args } = &e.kind {
                    if transfer_arg(args, aliases, steals, name) {
                        *stole = true;
                    }
                }
            }
            IrStmt::Expr(e) => {
                if let Some(target) = append_target(e) {
                    if aliases.contains(target) {
                        return false;
                    }
                }
            }
            IrStmt::ForRange { body, .. } | IrStmt::While { body, .. } => {
                let mut nested = aliases.clone();
                if !forward_stmts(body, param, &mut nested, steals, stole) {
                    return false;
                }
            }
            IrStmt::If { arms, else_block } => {
                for (_, b) in arms {
                    let mut nested = aliases.clone();
                    if !forward_stmts(b, param, &mut nested, steals, stole) {
                        return false;
                    }
                }
                if let Some(b) = else_block {
                    let mut nested = aliases.clone();
                    if !forward_stmts(b, param, &mut nested, steals, stole) {
                        return false;
                    }
                }
            }
            _ => {}
        }
        let _ = param;
    }
    true
}

fn mentions_later(stmts: &[IrStmt], name: &str) -> bool {
    // Used only as a coarse filter at the assignment site; the walker above
    // sees the whole body, so a second use simply fails to extend aliases
    // when this returns true. The assignment's own value is not in `stmts`
    // here — callers pass the enclosing slice, which may include the
    // assignment. Treat any mention as "later" only when it is not the
    // single initializer. Conservative: if unsure, don't alias.
    stmts.iter().filter(|s| stmt_mentions(s, name)).count() > 1
}

fn transfer_arg(
    args: &[IrExpr],
    aliases: &HashSet<String>,
    steals: &HashMap<String, HashSet<String>>,
    callee: &str,
) -> bool {
    let Some(stolen) = steals.get(callee) else {
        return false;
    };
    // The stolen parameter is the list being rebuilt, which the patterns
    // here always pass in first position.
    let Some(IrExprKind::Local(n)) = args.first().map(|a| &a.kind) else {
        return false;
    };
    aliases.contains(n) && stolen.iter().next().is_some() && {
        // Confirm the callee steals *some* list; the first arg is that list
        // for both rebuilds and the forwarders we recognize.
        let _ = stolen;
        true
    }
}

fn place_is_alias(s: &IrStmt, aliases: &HashSet<String>) -> bool {
    // A write through an alias that is not the transfer assignment above.
    match s {
        IrStmt::Assign {
            target: Place::Index { base, .. } | Place::Field { base, .. },
            ..
        } => place_root_in(base, aliases),
        _ => false,
    }
}

fn place_root_in(p: &Place, aliases: &HashSet<String>) -> bool {
    match p {
        Place::Var(n) => aliases.contains(n),
        Place::Index { base, .. } | Place::Field { base, .. } => place_root_in(base, aliases),
    }
}

/// Rename locals in a cloned helper body so inlining cannot capture the caller.
pub(crate) fn rename_body(body: &[IrStmt], map: &HashMap<String, String>) -> Vec<IrStmt> {
    body.iter().map(|s| rename_stmt(s, map)).collect()
}

fn rename_stmt(s: &IrStmt, map: &HashMap<String, String>) -> IrStmt {
    let rn = |n: &str| map.get(n).cloned().unwrap_or_else(|| n.to_string());
    match s {
        IrStmt::Assign {
            target,
            value,
            declares,
        } => IrStmt::Assign {
            target: rename_place(target, map),
            value: rename_expr(value, map),
            declares: *declares,
        },
        IrStmt::TupleAssign {
            targets,
            declares,
            value,
        } => IrStmt::TupleAssign {
            targets: targets.iter().map(|t| rn(t)).collect(),
            declares: declares.clone(),
            value: rename_expr(value, map),
        },
        IrStmt::Expr(e) => IrStmt::Expr(rename_expr(e, map)),
        IrStmt::If { arms, else_block } => IrStmt::If {
            arms: arms
                .iter()
                .map(|(c, b)| (rename_expr(c, map), rename_body(b, map)))
                .collect(),
            else_block: else_block.as_ref().map(|b| rename_body(b, map)),
        },
        IrStmt::Return(v) => IrStmt::Return(v.as_ref().map(|e| rename_expr(e, map))),
        IrStmt::Assert { cond, line } => IrStmt::Assert {
            cond: rename_expr(cond, map),
            line: *line,
        },
        other => other.clone(),
    }
}

fn rename_place(p: &Place, map: &HashMap<String, String>) -> Place {
    match p {
        Place::Var(n) => Place::Var(map.get(n).cloned().unwrap_or_else(|| n.clone())),
        Place::Index {
            base,
            base_ty,
            index,
        } => Place::Index {
            base: Box::new(rename_place(base, map)),
            base_ty: base_ty.clone(),
            index: Box::new(rename_expr(index, map)),
        },
        Place::Field {
            base,
            base_ty,
            name,
        } => Place::Field {
            base: Box::new(rename_place(base, map)),
            base_ty: base_ty.clone(),
            name: name.clone(),
        },
    }
}

fn rename_expr(e: &IrExpr, map: &HashMap<String, String>) -> IrExpr {
    let kind = match &e.kind {
        IrExprKind::Local(n) => IrExprKind::Local(map.get(n).cloned().unwrap_or_else(|| n.clone())),
        IrExprKind::List(xs) => IrExprKind::List(xs.iter().map(|x| rename_expr(x, map)).collect()),
        IrExprKind::Tuple(xs) => {
            IrExprKind::Tuple(xs.iter().map(|x| rename_expr(x, map)).collect())
        }
        IrExprKind::CallFunc { name, args } => IrExprKind::CallFunc {
            name: name.clone(),
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::CallValue { callee, args } => IrExprKind::CallValue {
            callee: Box::new(rename_expr(callee, map)),
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::NewRecord { name, args } => IrExprKind::NewRecord {
            name: name.clone(),
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::NewVariant {
            enum_name,
            variant,
            args,
        } => IrExprKind::NewVariant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::Builtin { builtin, args } => IrExprKind::Builtin {
            builtin: *builtin,
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::MutBuiltin {
            builtin,
            recv,
            recv_ty,
            args,
        } => IrExprKind::MutBuiltin {
            builtin: *builtin,
            recv: rename_place(recv, map),
            recv_ty: recv_ty.clone(),
            args: args.iter().map(|x| rename_expr(x, map)).collect(),
        },
        IrExprKind::GetField { recv, name } => IrExprKind::GetField {
            recv: Box::new(rename_expr(recv, map)),
            name: name.clone(),
        },
        IrExprKind::Index { recv, index } => IrExprKind::Index {
            recv: Box::new(rename_expr(recv, map)),
            index: Box::new(rename_expr(index, map)),
        },
        IrExprKind::Unary { op, operand } => IrExprKind::Unary {
            op: *op,
            operand: Box::new(rename_expr(operand, map)),
        },
        IrExprKind::Binary { op, lhs, rhs } => IrExprKind::Binary {
            op: *op,
            lhs: Box::new(rename_expr(lhs, map)),
            rhs: Box::new(rename_expr(rhs, map)),
        },
        other => other.clone(),
    };
    IrExpr {
        ty: e.ty.clone(),
        kind,
    }
}

/// An argument that can be copied into the helper body by name. Repeating
/// it does not repeat a side effect, and it is not a callee local.
pub(crate) fn subst_pure(e: &IrExpr) -> bool {
    match &e.kind {
        IrExprKind::Local(_) | IrExprKind::Int(_) | IrExprKind::Bool(_) => true,
        IrExprKind::Unary {
            op: sudoc_ir::UnaryOp::Neg,
            operand,
        } => matches!(operand.kind, IrExprKind::Int(_)),
        IrExprKind::GetField { recv, .. } => subst_pure(recv),
        _ => false,
    }
}

/// Replace `Local` names with the caller's expressions. One shot: the
/// replacement is not walked again, so a caller local keeps its name.
pub(crate) fn subst_locals(stmts: &[IrStmt], subst: &HashMap<String, IrExpr>) -> Vec<IrStmt> {
    stmts.iter().map(|s| subst_stmt(s, subst)).collect()
}

fn subst_stmt(s: &IrStmt, subst: &HashMap<String, IrExpr>) -> IrStmt {
    match s {
        IrStmt::Assign {
            target,
            value,
            declares,
        } => IrStmt::Assign {
            target: subst_place(target, subst),
            value: subst_expr(value, subst),
            declares: *declares,
        },
        IrStmt::TupleAssign {
            targets,
            declares,
            value,
        } => IrStmt::TupleAssign {
            targets: targets.clone(),
            declares: declares.clone(),
            value: subst_expr(value, subst),
        },
        IrStmt::Expr(e) => IrStmt::Expr(subst_expr(e, subst)),
        IrStmt::If { arms, else_block } => IrStmt::If {
            arms: arms
                .iter()
                .map(|(c, b)| (subst_expr(c, subst), subst_locals(b, subst)))
                .collect(),
            else_block: else_block.as_ref().map(|b| subst_locals(b, subst)),
        },
        IrStmt::Return(v) => IrStmt::Return(v.as_ref().map(|e| subst_expr(e, subst))),
        IrStmt::Assert { cond, line } => IrStmt::Assert {
            cond: subst_expr(cond, subst),
            line: *line,
        },
        other => other.clone(),
    }
}

fn subst_place(p: &Place, subst: &HashMap<String, IrExpr>) -> Place {
    match p {
        Place::Index {
            base,
            base_ty,
            index,
        } => Place::Index {
            base: Box::new(subst_place(base, subst)),
            base_ty: base_ty.clone(),
            index: Box::new(subst_expr(index, subst)),
        },
        Place::Field {
            base,
            base_ty,
            name,
        } => Place::Field {
            base: Box::new(subst_place(base, subst)),
            base_ty: base_ty.clone(),
            name: name.clone(),
        },
        other => other.clone(),
    }
}

fn subst_expr(e: &IrExpr, subst: &HashMap<String, IrExpr>) -> IrExpr {
    if let IrExprKind::Local(n) = &e.kind {
        if let Some(rep) = subst.get(n) {
            return rep.clone();
        }
    }
    let kind = match &e.kind {
        IrExprKind::List(xs) => IrExprKind::List(xs.iter().map(|x| subst_expr(x, subst)).collect()),
        IrExprKind::Tuple(xs) => {
            IrExprKind::Tuple(xs.iter().map(|x| subst_expr(x, subst)).collect())
        }
        IrExprKind::CallFunc { name, args } => IrExprKind::CallFunc {
            name: name.clone(),
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::CallValue { callee, args } => IrExprKind::CallValue {
            callee: Box::new(subst_expr(callee, subst)),
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::NewRecord { name, args } => IrExprKind::NewRecord {
            name: name.clone(),
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::NewVariant {
            enum_name,
            variant,
            args,
        } => IrExprKind::NewVariant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::Builtin { builtin, args } => IrExprKind::Builtin {
            builtin: *builtin,
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::MutBuiltin {
            builtin,
            recv,
            recv_ty,
            args,
        } => IrExprKind::MutBuiltin {
            builtin: *builtin,
            recv: subst_place(recv, subst),
            recv_ty: recv_ty.clone(),
            args: args.iter().map(|x| subst_expr(x, subst)).collect(),
        },
        IrExprKind::GetField { recv, name } => IrExprKind::GetField {
            recv: Box::new(subst_expr(recv, subst)),
            name: name.clone(),
        },
        IrExprKind::Index { recv, index } => IrExprKind::Index {
            recv: Box::new(subst_expr(recv, subst)),
            index: Box::new(subst_expr(index, subst)),
        },
        IrExprKind::Unary { op, operand } => IrExprKind::Unary {
            op: *op,
            operand: Box::new(subst_expr(operand, subst)),
        },
        IrExprKind::Binary { op, lhs, rhs } => IrExprKind::Binary {
            op: *op,
            lhs: Box::new(subst_expr(lhs, subst)),
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        other => other.clone(),
    };
    IrExpr {
        ty: e.ty.clone(),
        kind,
    }
}

pub(crate) fn declared_locals(stmts: &[IrStmt]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(stmts: &[IrStmt], out: &mut Vec<String>) {
        for s in stmts {
            match s {
                IrStmt::Assign {
                    target: Place::Var(n),
                    declares: true,
                    ..
                } => out.push(n.clone()),
                IrStmt::TupleAssign {
                    targets, declares, ..
                } => {
                    for (t, d) in targets.iter().zip(declares.iter()) {
                        if *d {
                            out.push(t.clone());
                        }
                    }
                }
                IrStmt::If { arms, else_block } => {
                    for (_, b) in arms {
                        walk(b, out);
                    }
                    if let Some(b) = else_block {
                        walk(b, out);
                    }
                }
                _ => {}
            }
        }
    }
    walk(stmts, &mut out);
    out
}
