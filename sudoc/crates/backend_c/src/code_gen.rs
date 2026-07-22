//! Function/statement/expression emission with the ownership discipline
//! described in lib.rs.
//!
//! Invariant: `eval` returns an *effect-free* C expression — every trapping
//! or observable-side-effect subexpression has already been materialized into
//! a named temporary (emitted lines, in sudo's strict left-to-right order),
//! so parent expressions can combine child code without C's unspecified
//! evaluation order becoming observable. Composite values are either borrows
//! (lvalue paths, copied when stored) or owned temporaries (freed at
//! statement end unless moved into storage).

use std::collections::HashSet;

use sudoc_ir::{
    BinaryOp, Builtin, IrExpr, IrExprKind, IrFunc, IrModule, IrPattern, IrStmt, IrTest,
    Place, Ty, UnaryOp,
};

use crate::types_gen::{boxed_in_payload, c_type, canon_of, copy_of, eq_of, is_scalar, mangle};

pub(crate) struct FnEmitter<'a> {
    pub m: &'a IrModule,
    pub out: &'a mut String,
    indent: usize,
    counter: u32,
    /// Scope stack of owned composite locals (name, type); loop boundaries
    /// marked so `break` can free the right scopes.
    scopes: Vec<Scope>,
    /// Inout parameters: accessed as `(*name)`.
    inouts: HashSet<String>,
    /// Owned temporaries of the current statement, freed at its end unless
    /// moved. (name, type)
    stmt_temps: Vec<(String, Ty)>,
    /// This function's export has a host wrapper, so it emits as static.
    static_export: bool,
    /// Innermost-first loop label state, for break/continue that must cross
    /// a C `switch` (where a bare `break` would bind to the switch).
    loops: Vec<LoopLabels>,
    /// Number of C `switch` statements currently open.
    switch_depth: u32,
}

struct LoopLabels {
    brk: String,
    cnt: String,
    brk_used: bool,
    cnt_used: bool,
    switch_depth_at_entry: u32,
}

struct Scope {
    owned: Vec<(String, Ty)>,
    is_loop: bool,
}

#[derive(Clone)]
pub(crate) struct CVal {
    code: String,
    kind: ValKind,
    ty: Ty,
}

#[derive(Clone, PartialEq)]
enum ValKind {
    /// Scalar (or function pointer): plain value expression.
    Scalar,
    /// Composite lvalue owned by someone else; copy when storing.
    Borrow,
    /// Composite named temporary owned by this statement.
    Owned,
}


/// Does evaluating this expression potentially trap? (Used to keep `and`/`or`
/// right operands lazy.)
fn can_trap(e: &IrExpr) -> bool {
    match &e.kind {
        IrExprKind::Index { .. }
        | IrExprKind::CallFunc { .. }
        | IrExprKind::CallValue { .. }
        | IrExprKind::MutBuiltin { .. } => true,
        IrExprKind::Binary { op: BinaryOp::Div | BinaryOp::Mod, lhs, .. }
            if matches!(lhs.ty, Ty::Int) =>
        {
            true
        }
        IrExprKind::Binary { lhs, rhs, .. } => can_trap(lhs) || can_trap(rhs),
        IrExprKind::Unary { operand, .. } => can_trap(operand),
        IrExprKind::Builtin { builtin, args } => {
            matches!(
                builtin,
                Builtin::OptUnwrap
                    | Builtin::ResUnwrap
                    | Builtin::IntOfFloat
                    | Builtin::Filled
            ) || args.iter().any(can_trap)
        }
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. } => xs.iter().any(can_trap),
        IrExprKind::GetField { recv, .. } => can_trap(recv),
        _ => false,
    }
}

fn uses_local(stmts: &[IrStmt], name: &str) -> bool {
    fn expr_uses(e: &IrExpr, name: &str) -> bool {
        match &e.kind {
            IrExprKind::Local(n) => n == name,
            IrExprKind::List(xs)
            | IrExprKind::Tuple(xs)
            | IrExprKind::CallFunc { args: xs, .. }
            | IrExprKind::NewRecord { args: xs, .. }
            | IrExprKind::NewVariant { args: xs, .. }
            | IrExprKind::Builtin { args: xs, .. } => xs.iter().any(|x| expr_uses(x, name)),
            IrExprKind::CallValue { callee, args } => {
                expr_uses(callee, name) || args.iter().any(|x| expr_uses(x, name))
            }
            IrExprKind::MutBuiltin { recv, args, .. } => {
                place_uses(recv, name) || args.iter().any(|x| expr_uses(x, name))
            }
            IrExprKind::GetField { recv, .. } => expr_uses(recv, name),
            IrExprKind::Index { recv, index } => {
                expr_uses(recv, name) || expr_uses(index, name)
            }
            IrExprKind::Unary { operand, .. } => expr_uses(operand, name),
            IrExprKind::Binary { lhs, rhs, .. } => {
                expr_uses(lhs, name) || expr_uses(rhs, name)
            }
            _ => false,
        }
    }
    fn place_uses(p: &Place, name: &str) -> bool {
        match p {
            Place::Var(n) => n == name,
            Place::Index { base, index, .. } => {
                place_uses(base, name) || expr_uses(index, name)
            }
            Place::Field { base, .. } => place_uses(base, name),
        }
    }
    stmts.iter().any(|s| match s {
        IrStmt::Assign { target, value, .. } => {
            place_uses(target, name) || expr_uses(value, name)
        }
        IrStmt::TupleAssign { targets, value, .. } => {
            targets.iter().any(|t| t == name) || expr_uses(value, name)
        }
        IrStmt::Expr(e) => expr_uses(e, name),
        IrStmt::If { arms, else_block } => {
            arms.iter().any(|(c, b)| expr_uses(c, name) || uses_local(b, name))
                || else_block.as_ref().is_some_and(|b| uses_local(b, name))
        }
        IrStmt::While { cond, body } => expr_uses(cond, name) || uses_local(body, name),
        IrStmt::ForRange { from, to, body, .. } => {
            expr_uses(from, name) || expr_uses(to, name) || uses_local(body, name)
        }
        IrStmt::ForIn { iter, body, .. } => expr_uses(iter, name) || uses_local(body, name),
        IrStmt::Match { scrutinee, arms } => {
            expr_uses(scrutinee, name) || arms.iter().any(|a| uses_local(&a.body, name))
        }
        IrStmt::Return(Some(e)) => expr_uses(e, name),
        IrStmt::Assert { cond, .. } => expr_uses(cond, name),
        IrStmt::ExpectTrap { body, .. } => uses_local(body, name),
        IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => false,
    })
}

fn int_lit(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".into()
    } else if v < 0 {
        format!("({v}LL)")
    } else {
        format!("{v}LL")
    }
}

fn managed(ty: &Ty) -> bool {
    !is_scalar(ty) && !matches!(ty, Ty::Func { .. })
}


/// `&(*p)` is just `p`; keep generated code free of the double dance.
fn addr_of(lv: &str) -> String {
    if let Some(inner) = lv.strip_prefix("(*").and_then(|r| r.strip_suffix(")")) {
        if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return inner.to_string();
        }
    }
    format!("&{lv}")
}

/// Strip one redundant outermost paren pair (`if ((a == b))` upsets clang).
fn trim_parens(code: &str) -> &str {
    let bytes = code.as_bytes();
    if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
        return code;
    }
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && i != bytes.len() - 1 {
                    return code; // outer parens close early: not redundant
                }
            }
            _ => {}
        }
    }
    &code[1..code.len() - 1]
}

impl<'a> FnEmitter<'a> {
    pub(crate) fn new(m: &'a IrModule, out: &'a mut String) -> Self {
        FnEmitter {
            m,
            out,
            indent: 0,
            counter: 0,
            scopes: Vec::new(),
            inouts: HashSet::new(),
            stmt_temps: Vec::new(),
            static_export: false,
            loops: Vec::new(),
            switch_depth: 0,
        }
    }

    pub(crate) fn set_static_export(&mut self, yes: bool) {
        self.static_export = yes;
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn fresh(&mut self) -> String {
        let n = format!("_t{}", self.counter);
        self.counter += 1;
        n
    }

    // ---- functions --------------------------------------------------------

    pub(crate) fn emit_func(&mut self, f: &IrFunc) {
        let ret = f.ret.as_ref().map(c_type).unwrap_or_else(|| "void".into());
        let linkage =
            if f.export && !self.static_export { "" } else { "SUDO_UNUSED static " };
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                if p.inout {
                    format!("{} *{}", c_type(&p.ty), p.name)
                } else {
                    format!("{} {}", c_type(&p.ty), p.name)
                }
            })
            .collect();
        let args = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        self.line(&format!("{linkage}{ret} {}({args}) {{", f.name));
        self.indent += 1;
        self.counter = 0;
        self.inouts = f.params.iter().filter(|p| p.inout).map(|p| p.name.clone()).collect();
        self.scopes.push(Scope { owned: Vec::new(), is_loop: false });
        for p in &f.params {
            if !uses_local(&f.body, &p.name) {
                self.line(&format!("(void){};", p.name));
            }
            if !p.inout && managed(&p.ty) {
                self.scopes.last_mut().unwrap().owned.push((p.name.clone(), p.ty.clone()));
            }
        }
        let terminated = self.emit_block_stmts(&f.body);
        if !terminated {
            self.free_scope_frame();
        }
        self.scopes.pop();
        if f.ret.is_some() && !terminated {
            self.line("sudo_unreachable();");
        }
        self.indent -= 1;
        self.line("}");
        self.line("");
    }

    pub(crate) fn emit_test(&mut self, t: &IrTest, c_name: &str) {
        self.line(&format!("static void {c_name}(void) {{"));
        self.indent += 1;
        self.counter = 0;
        self.inouts.clear();
        self.scopes.push(Scope { owned: Vec::new(), is_loop: false });
        let terminated = self.emit_block_stmts(&t.body);
        if !terminated {
            self.free_scope_frame();
        }
        self.scopes.pop();
        self.indent -= 1;
        self.line("}");
        self.line("");
    }

    /// Emit statements of the current scope frame. Returns true if the block
    /// definitely terminated (return/break) so scope frees were already done.
    fn emit_block_stmts(&mut self, stmts: &[IrStmt]) -> bool {
        for s in stmts {
            self.emit_stmt(s);
            if matches!(
                s,
                IrStmt::Return(_) | IrStmt::Break | IrStmt::Continue | IrStmt::ExpectTrap { .. }
            ) {
                return true; // anything after is unreachable (or forbidden)
            }
        }
        false
    }

    /// A nested block with its own scope: emits frees at exit unless the
    /// block terminated by itself.
    fn emit_block(&mut self, stmts: &[IrStmt], is_loop: bool) {
        self.scopes.push(Scope { owned: Vec::new(), is_loop });
        let terminated = self.emit_block_stmts(stmts);
        if !terminated {
            self.free_scope_frame();
        }
        self.scopes.pop();
    }

    fn free_scope_frame(&mut self) {
        let frees: Vec<String> = self
            .scopes
            .last()
            .unwrap()
            .owned
            .iter()
            .rev()
            .map(|(n, t)| self.free_stmt_for(n, t))
            .collect();
        for f in frees {
            self.line(&f);
        }
    }

    fn free_stmt_for(&self, name: &str, ty: &Ty) -> String {
        let lv = self.local_lvalue(name);
        format!("{}_free({});", mangle(ty), addr_of(&lv))
    }

    fn local_lvalue(&self, name: &str) -> String {
        if self.inouts.contains(name) {
            format!("(*{name})")
        } else {
            name.to_string()
        }
    }

    /// End-of-statement: free owned temps that were not moved.
    fn flush_stmt_temps(&mut self) {
        let temps: Vec<(String, Ty)> = self.stmt_temps.drain(..).rev().collect();
        for (n, t) in temps {
            self.line(&format!("{}_free(&{n});", mangle(&t)));
        }
    }

    // ---- statements -------------------------------------------------------

    fn emit_stmt(&mut self, s: &IrStmt) {
        match s {
            IrStmt::Assign { target, value, declares } => {
                self.emit_assign(target, value, *declares);
                self.flush_stmt_temps();
            }
            IrStmt::TupleAssign { targets, declares, value } => {
                self.emit_tuple_assign(targets, declares, value);
                self.flush_stmt_temps();
            }
            IrStmt::Expr(e) => {
                self.emit_expr_stmt(e);
                self.flush_stmt_temps();
            }
            IrStmt::If { arms, else_block } => self.emit_if(arms, else_block.as_deref(), 0),
            IrStmt::While { cond, body } => self.emit_while(cond, body),
            IrStmt::ForRange { var, from, to, down, body } => {
                self.emit_for_range(var, from, to, *down, body)
            }
            IrStmt::ForIn { vars, iter, body } => self.emit_for_in(vars, iter, body),
            IrStmt::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
            IrStmt::Return(v) => self.emit_return(v.as_ref()),
            IrStmt::Assert { cond, line } => {
                // Equality asserts serialize their operands on failure
                // (harness diagnostics, lockstep.md §3).
                if let IrExprKind::Binary { op: sudoc_ir::BinaryOp::Eq, lhs, rhs } = &cond.kind
                {
                    let l = self.eval(lhs);
                    let r = self.eval(rhs);
                    let lp = self.val_ptr(&l, &lhs.ty);
                    let rp = self.val_ptr(&r, &rhs.ty);
                    let eq = if is_scalar(&lhs.ty) {
                        format!("({} == {})", l.code, r.code)
                    } else {
                        eq_of(&lhs.ty, &lp, &rp)
                    };
                    self.line(&format!("if (!{eq}) {{"));
                    self.indent += 1;
                    self.line("sudo_det_reset();");
                    self.line(&format!("sudo_det_str(\"line {line}: \");"));
                    self.line(&canon_of(&lhs.ty, &lp));
                    self.line("sudo_det_str(\" != \");");
                    self.line(&canon_of(&rhs.ty, &rp));
                    self.line(&format!("sudo_trap(SUDO_TRAP_ASSERT_FAILED, {line});"));
                    self.indent -= 1;
                    self.line("}");
                } else {
                    let c = self.eval(cond);
                    self.line(&format!("sudo_assert({}, {line});", c.code));
                }
                self.flush_stmt_temps();
            }
            IrStmt::Skip => self.line(";"),
            IrStmt::Break => self.emit_loop_exit(true),
            IrStmt::Continue => self.emit_loop_exit(false),
            IrStmt::ExpectTrap { kind, body, line } => self.emit_expect_trap(kind, body, *line),
        }
    }

    /// `break`/`continue`: free scopes inward through the loop body, then
    /// jump — via goto when a C switch would swallow the plain keyword.
    fn emit_loop_exit(&mut self, is_break: bool) {
        let mut frees = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (n, t) in scope.owned.iter().rev() {
                frees.push(self.free_stmt_for(n, t));
            }
            if scope.is_loop {
                break;
            }
        }
        for f in frees {
            self.line(&f);
        }
        let crossing = self
            .loops
            .last()
            .map(|l| self.switch_depth > l.switch_depth_at_entry)
            .unwrap_or(false);
        if crossing {
            let label = {
                let top = self.loops.last_mut().expect("checker: inside a loop");
                if is_break {
                    top.brk_used = true;
                    top.brk.clone()
                } else {
                    top.cnt_used = true;
                    top.cnt.clone()
                }
            };
            self.line(&format!("goto {label};"));
        } else if is_break {
            self.line("break;");
        } else {
            self.line("continue;");
        }
    }

    fn push_loop(&mut self) {
        let n = self.loops.len();
        self.loops.push(LoopLabels {
            brk: format!("_brk{n}"),
            cnt: format!("_cnt{n}"),
            brk_used: false,
            cnt_used: false,
            switch_depth_at_entry: self.switch_depth,
        });
    }

    /// Close the loop body: emit the continue label (inside the loop, so
    /// control falls into the increment/condition) — call before `}`.
    fn emit_cnt_label(&mut self) {
        if self.loops.last().is_some_and(|l| l.cnt_used) {
            let label = self.loops.last().unwrap().cnt.clone();
            self.line(&format!("{label}: ;"));
        }
    }

    /// After the loop (and any post-loop cleanup): the break label.
    fn pop_loop_emit_brk(&mut self) {
        let top = self.loops.pop().expect("balanced loop stack");
        if top.brk_used {
            self.line(&format!("{}: ;", top.brk));
        }
    }

    fn emit_expect_trap(&mut self, kind: &str, body: &[IrStmt], line: u32) {
        let status = match kind {
            "OutOfBounds" => "SUDO_TRAP_OUT_OF_BOUNDS",
            "KeyMissing" => "SUDO_TRAP_KEY_MISSING",
            "DivByZero" => "SUDO_TRAP_DIV_BY_ZERO",
            "Overflow" => "SUDO_TRAP_OVERFLOW",
            "UnwrapFailed" => "SUDO_TRAP_UNWRAP_FAILED",
            "InvalidConvert" => "SUDO_TRAP_INVALID_CONVERT",
            "AssertFailed" => "SUDO_TRAP_ASSERT_FAILED",
            _ => "SUDO_TRAP_INVALID_ARG",
        };
        self.line("{");
        self.indent += 1;
        self.line("jmp_buf _saved;");
        self.line("memcpy(&_saved, &sudo_trap_jmp, sizeof(jmp_buf));");
        self.line("if (setjmp(sudo_trap_jmp) == 0) {");
        self.indent += 1;
        self.emit_block(body, false);
        self.line("memcpy(&sudo_trap_jmp, &_saved, sizeof(jmp_buf));");
        self.line("sudo_det_reset();");
        self.line(&format!(
            "sudo_det_str(\"line {line}: expected trap {kind}, but nothing trapped\");"
        ));
        self.line(&format!("sudo_trap(SUDO_TRAP_ASSERT_FAILED, {line});"));
        self.indent -= 1;
        self.line("} else {");
        self.indent += 1;
        self.line("memcpy(&sudo_trap_jmp, &_saved, sizeof(jmp_buf));");
        self.line(&format!("if (sudo_trap_status != {status}) {{"));
        self.indent += 1;
        self.line("sudo_det_reset();");
        self.line(&format!("sudo_det_str(\"line {line}: expected trap {kind}, got \");"));
        self.line("sudo_det_str(sudo_status_name(sudo_trap_status));");
        self.line(&format!("sudo_trap(SUDO_TRAP_ASSERT_FAILED, {line});"));
        self.indent -= 1;
        self.line("}");
        self.line("return;");
        self.indent -= 1;
        self.line("}");
        self.indent -= 1;
        self.line("}");
    }

    fn emit_assign(&mut self, target: &Place, value: &IrExpr, declares: bool) {
        // Map index-assignment is insert-or-overwrite, not slot mutation.
        if let Place::Index { base, base_ty, index } = target {
            if matches!(base_ty, Ty::Map(..)) {
                let (k_ty, _) = match base_ty {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                let v_code = self.store(value);
                let k_code = self.store_expr_of(index, &k_ty);
                let base_lv = self.place_lvalue(base);
                self.line(&format!(
                    "{}_put({}, {k_code}, {v_code});",
                    mangle(base_ty),
                    addr_of(&base_lv)
                ));
                return;
            }
        }
        match target {
            Place::Var(name) if declares => {
                let code = self.store(value);
                let lv = self.local_lvalue(name);
                self.line(&format!("{} {lv} = {code};", c_type(&value.ty)));
                if managed(&value.ty) {
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .owned
                        .push((name.clone(), value.ty.clone()));
                }
            }
            _ => {
                // Reassignment (or slot/field write): RHS first, then free
                // the old value, then move in.
                if managed(&value.ty) {
                    let code = self.store(value);
                    let tmp = self.fresh();
                    let ct = c_type(&value.ty);
                    self.line(&format!("{ct} {tmp} = {code};"));
                    let lv = self.place_lvalue(target);
                    self.line(&format!("{}_free({});", mangle(&value.ty), addr_of(&lv)));
                    self.line(&format!("{lv} = {tmp};"));
                } else {
                    let code = self.store(value);
                    let lv = self.place_lvalue(target);
                    self.line(&format!("{lv} = {code};"));
                }
            }
        }
    }

    fn emit_tuple_assign(&mut self, targets: &[String], declares: &[bool], value: &IrExpr) {
        let ts = match &value.ty {
            Ty::Tuple(ts) => ts.clone(),
            _ => unreachable!("tuple assign from non-tuple"),
        };
        let code = self.store(value);
        let tmp = self.fresh();
        self.line(&format!("{} {tmp} = {code};", c_type(&value.ty)));
        // Fields are moved out one by one; the tuple itself is not freed.
        for ((name, decl), ty) in targets.iter().zip(declares).zip(&ts) {
            let i = targets.iter().position(|t| t == name).unwrap();
            let lv = self.local_lvalue(name);
            if *decl {
                self.line(&format!("{} {lv} = {tmp}.f{i};", c_type(ty)));
                if managed(ty) {
                    self.scopes.last_mut().unwrap().owned.push((name.clone(), ty.clone()));
                }
            } else if managed(ty) {
                self.line(&format!("{}_free({});", mangle(ty), addr_of(&lv)));
                self.line(&format!("{lv} = {tmp}.f{i};"));
            } else {
                self.line(&format!("{lv} = {tmp}.f{i};"));
            }
        }
    }

    fn emit_expr_stmt(&mut self, e: &IrExpr) {
        let v = self.eval(e);
        if v.kind == ValKind::Owned {
            // Result discarded; the temp free at statement end handles it.
            return;
        }
        if v.code.is_empty() {
            return;
        }
        // Void calls emit their own line in eval; scalar results discard.
        if v.code != "/*void*/" {
            self.line(&format!("(void)({});", v.code));
        }
    }

    fn emit_if(&mut self, arms: &[(IrExpr, Vec<IrStmt>)], else_block: Option<&[IrStmt]>, idx: usize) {
        let (cond, body) = &arms[idx];
        let before = self.stmt_temps.len();
        let c = self.eval(cond);
        let cond_code = if self.stmt_temps.len() > before {
            // Materialize so cond temps can be freed before branching.
            let tmp = self.fresh();
            self.line(&format!("bool {tmp} = {};", c.code));
            let temps: Vec<(String, Ty)> = self.stmt_temps.drain(before..).rev().collect();
            for (n, t) in temps {
                self.line(&format!("{}_free(&{n});", mangle(&t)));
            }
            tmp
        } else {
            c.code
        };
        self.line(&format!("if ({}) {{", trim_parens(&cond_code)));
        self.indent += 1;
        self.emit_block(body, false);
        self.indent -= 1;
        if idx + 1 < arms.len() {
            self.line("} else {");
            self.indent += 1;
            self.emit_if(arms, else_block, idx + 1);
            self.indent -= 1;
            self.line("}");
        } else if let Some(eb) = else_block {
            self.line("} else {");
            self.indent += 1;
            self.emit_block(eb, false);
            self.indent -= 1;
            self.line("}");
        } else {
            self.line("}");
        }
    }

    fn emit_while(&mut self, cond: &IrExpr, body: &[IrStmt]) {
        // Plain form when the condition needs no statement lines.
        let probe = String::new();
        let mut probe_emitter = String::new();
        let _ = probe;
        let simple = {
            // Dry-run the condition into a scratch buffer.
            let mut scratch = FnEmitter {
                m: self.m,
                out: &mut probe_emitter,
                indent: 0,
                counter: self.counter,
                scopes: Vec::new(),
                inouts: self.inouts.clone(),
                stmt_temps: Vec::new(),
                static_export: false,
                loops: Vec::new(),
                switch_depth: 0,
            };
            scratch.scopes.push(Scope { owned: Vec::new(), is_loop: false });
            let v = scratch.eval(cond);
            probe_emitter.is_empty().then_some(v.code)
        };
        if let Some(code) = simple {
            self.push_loop();
            self.line(&format!("while ({}) {{", trim_parens(&code)));
            self.indent += 1;
            self.emit_block(body, true);
            self.emit_cnt_label();
            self.indent -= 1;
            self.line("}");
            self.pop_loop_emit_brk();
            return;
        }
        self.push_loop();
        self.line("for (;;) {");
        self.indent += 1;
        self.scopes.push(Scope { owned: Vec::new(), is_loop: true });
        let before = self.stmt_temps.len();
        let c = self.eval(cond);
        let tmp = self.fresh();
        self.line(&format!("bool {tmp} = {};", c.code));
        let temps: Vec<(String, Ty)> = self.stmt_temps.drain(before..).rev().collect();
        for (n, t) in temps {
            self.line(&format!("{}_free(&{n});", mangle(&t)));
        }
        self.line(&format!("if (!{tmp}) break;"));
        let terminated = self.emit_block_stmts(body);
        if !terminated {
            self.free_scope_frame();
        }
        self.scopes.pop();
        self.emit_cnt_label();
        self.indent -= 1;
        self.line("}");
        self.pop_loop_emit_brk();
    }

    fn emit_for_range(&mut self, var: &str, from: &IrExpr, to: &IrExpr, down: bool, body: &[IrStmt]) {
        let f = self.eval(from);
        let t = self.eval(to);
        let lo = self.fresh();
        let hi = self.fresh();
        self.line(&format!("int64_t {lo} = {};", f.code));
        self.line(&format!("int64_t {hi} = {};", t.code));
        self.flush_stmt_temps();
        // Overflow-safe and continue-safe: the "was this the last
        // iteration?" flag is computed at the top of the body, the increment
        // wraps harmlessly through unsigned arithmetic in the update slot.
        let done = self.fresh();
        let (cmp, step) = if down {
            (">=", format!("{var} = (int64_t)((uint64_t){var} - 1)"))
        } else {
            ("<=", format!("{var} = (int64_t)((uint64_t){var} + 1)"))
        };
        self.line(&format!("bool {done} = !({lo} {cmp} {hi});"));
        self.push_loop();
        self.line(&format!("for (int64_t {var} = {lo}; !{done}; {step}) {{"));
        self.indent += 1;
        self.line(&format!("{done} = ({var} == {hi});"));
        if !uses_local(body, var) {
            self.line(&format!("(void){var};"));
        }
        self.emit_block(body, true);
        self.emit_cnt_label();
        self.indent -= 1;
        self.line("}");
        self.pop_loop_emit_brk();
    }

    fn emit_for_in(&mut self, vars: &[String], iter: &IrExpr, body: &[IrStmt]) {
        // Iterate over a copy taken at loop entry (spec §5.3).
        let it = self.eval(iter);
        let snap = self.fresh();
        let it_ty = iter.ty.clone();
        let n = mangle(&it_ty);
        let ct = c_type(&it_ty);
        match it.kind {
            ValKind::Owned => {
                // Already a fresh value; iterate it directly.
                self.line(&format!("{ct} {snap} = {};", it.code));
                let pos = self.stmt_temps.iter().position(|(t, _)| *t == it.code);
                if let Some(pos) = pos {
                    self.stmt_temps.remove(pos);
                }
            }
            _ => {
                self.line(&format!("{ct} {snap} = {n}_copy({});", addr_of(&it.code)));
            }
        }
        self.flush_stmt_temps();
        self.push_loop();
        let idx = self.fresh();
        match &it_ty {
            Ty::List(e) => {
                self.line(&format!("for (int64_t {idx} = 0; {idx} < {snap}.len; {idx}++) {{"));
                self.indent += 1;
                let v = &vars[0];
                self.line(&format!("{} {v} = {snap}.data[{idx}];", c_type(e)));
                if !uses_local(body, v) {
                    self.line(&format!("(void){v};"));
                }
            }
            Ty::Set(e) => {
                self.line(&format!("for (int64_t {idx} = 0; {idx} < {snap}.cap; {idx}++) {{"));
                self.indent += 1;
                self.line(&format!("if ({snap}.entries[{idx}].state != 1) continue;"));
                let v = &vars[0];
                self.line(&format!("{} {v} = {snap}.entries[{idx}].item;", c_type(e)));
                if !uses_local(body, v) {
                    self.line(&format!("(void){v};"));
                }
            }
            Ty::Map(k, val) => {
                self.line(&format!("for (int64_t {idx} = 0; {idx} < {snap}.cap; {idx}++) {{"));
                self.indent += 1;
                self.line(&format!("if ({snap}.entries[{idx}].state != 1) continue;"));
                self.line(&format!("{} {} = {snap}.entries[{idx}].key;", c_type(k), vars[0]));
                self.line(&format!("{} {} = {snap}.entries[{idx}].value;", c_type(val), vars[1]));
                for v in vars {
                    if !uses_local(body, v) {
                        self.line(&format!("(void){v};"));
                    }
                }
            }
            _ => unreachable!("cannot iterate {it_ty:?}"),
        }
        self.emit_block(body, true);
        self.emit_cnt_label();
        self.indent -= 1;
        self.line("}");
        self.line(&format!("{n}_free(&{snap});"));
        self.pop_loop_emit_brk();
    }

    fn emit_match(&mut self, scrutinee: &IrExpr, arms: &[sudoc_ir::IrMatchArm]) {
        let sc = self.eval(scrutinee);
        let sc_ty = scrutinee.ty.clone();
        match &sc_ty {
            Ty::Int => {
                let mut wild: Option<&sudoc_ir::IrMatchArm> = None;
                let mut seen_ints: HashSet<i64> = HashSet::new();
                self.switch_depth += 1;
                self.line(&format!("switch ({}) {{", sc.code));
                for arm in arms {
                    match &arm.pattern {
                        IrPattern::Int(v) => {
                            // First-matching-case-wins (spec §6.3): a later
                            // arm whose literal already appeared is dead
                            // code. A C `switch` can't express that with a
                            // duplicate `case` label, so drop it.
                            if !seen_ints.insert(*v) {
                                continue;
                            }
                            self.line(&format!("case {}: {{", int_lit(*v)));
                            self.indent += 1;
                            self.emit_block(&arm.body, false);
                            self.line("break;");
                            self.indent -= 1;
                            self.line("}");
                        }
                        IrPattern::Wildcard => wild = Some(arm),
                        _ => unreachable!(),
                    }
                }
                if let Some(arm) = wild {
                    self.line("default: {");
                    self.indent += 1;
                    self.emit_block(&arm.body, false);
                    self.line("break;");
                    self.indent -= 1;
                    self.line("}");
                }
                self.line("}");
                self.switch_depth -= 1;
            }
            Ty::Bool => {
                // if/else chain over true/false/wildcard.
                let mut first = true;
                for arm in arms {
                    let cond = match &arm.pattern {
                        IrPattern::Bool(true) => format!("({})", sc.code),
                        IrPattern::Bool(false) => format!("(!({}))", sc.code),
                        IrPattern::Wildcard => "".to_string(),
                        _ => unreachable!(),
                    };
                    if cond.is_empty() || !first && arm.pattern == IrPattern::Wildcard {
                        self.line("} else {");
                    } else if first {
                        self.line(&format!("if ({cond}) {{"));
                    } else {
                        self.line(&format!("}} else if ({cond}) {{"));
                    }
                    self.indent += 1;
                    self.emit_block(&arm.body, false);
                    self.indent -= 1;
                    first = false;
                }
                self.line("}");
            }
            Ty::Option_(inner) => {
                let payload = if boxed_in_payload(inner) {
                    format!("(*({}).value)", sc.code)
                } else {
                    format!("({}).value", sc.code)
                };
                self.emit_two_way_match(
                    arms,
                    &format!("({}).has", sc.code),
                    "Some",
                    &[(inner.as_ref().clone(), payload)],
                    "None",
                    &[],
                );
            }
            Ty::Result_(t, e) => {
                let ok = if boxed_in_payload(t) {
                    format!("(*({}).ok)", sc.code)
                } else {
                    format!("({}).ok", sc.code)
                };
                let err = if boxed_in_payload(e) {
                    format!("(*({}).err)", sc.code)
                } else {
                    format!("({}).err", sc.code)
                };
                self.emit_two_way_match(
                    arms,
                    &format!("({}).is_ok", sc.code),
                    "Ok",
                    &[(t.as_ref().clone(), ok)],
                    "Err",
                    &[(e.as_ref().clone(), err)],
                );
            }
            Ty::Enum(ename) => {
                let en = self.m.enum_(ename).expect("enum exists").clone();
                self.switch_depth += 1;
                self.line(&format!("switch (({}).tag) {{", sc.code));
                let mut wild: Option<&sudoc_ir::IrMatchArm> = None;
                for arm in arms {
                    match &arm.pattern {
                        IrPattern::Variant { variant, binders, .. } => {
                            let v = en.variants.iter().find(|v| v.name == *variant).unwrap();
                            self.line(&format!("case {}: {{", crate::types_gen::tag_name(ename, variant)));
                            self.indent += 1;
                            self.scopes.push(Scope { owned: Vec::new(), is_loop: false });
                            for (b, (fname, fty)) in binders.iter().zip(&v.fields) {
                                let slot = if boxed_in_payload(fty) {
                                    format!("(*({}).as.{variant}.{fname})", sc.code)
                                } else {
                                    format!("({}).as.{variant}.{fname}", sc.code)
                                };
                                self.bind_pattern_var(b, fty, &slot, &arm.body);
                            }
                            let terminated = self.emit_block_stmts(&arm.body);
                            if !terminated {
                                self.free_scope_frame();
                            }
                            self.scopes.pop();
                            if !terminated {
                                self.line("break;");
                            }
                            self.indent -= 1;
                            self.line("}");
                        }
                        IrPattern::Wildcard => wild = Some(arm),
                        _ => unreachable!(),
                    }
                }
                if let Some(arm) = wild {
                    self.line("default: {");
                    self.indent += 1;
                    self.emit_block(&arm.body, false);
                    self.line("break;");
                    self.indent -= 1;
                    self.line("}");
                }
                self.line("}");
                self.switch_depth -= 1;
            }
            other => unreachable!("cannot match on {other:?}"),
        }
        self.flush_stmt_temps();
    }

    /// Option/Result matches: an if/else on the flag with copied binders.
    #[allow(clippy::too_many_arguments)]
    fn emit_two_way_match(
        &mut self,
        arms: &[sudoc_ir::IrMatchArm],
        flag: &str,
        yes_name: &str,
        yes_payloads: &[(Ty, String)],
        no_name: &str,
        no_payloads: &[(Ty, String)],
    ) {
        let find = |name: &str| {
            arms.iter().find(|a| {
                matches!(&a.pattern, IrPattern::Variant { variant, .. } if variant == name)
            })
        };
        let wild = arms.iter().find(|a| a.pattern == IrPattern::Wildcard);
        let yes = find(yes_name);
        let no = find(no_name);

        self.line(&format!("if ({flag}) {{"));
        self.indent += 1;
        if let Some(arm) = yes {
            self.scopes.push(Scope { owned: Vec::new(), is_loop: false });
            if let IrPattern::Variant { binders, .. } = &arm.pattern {
                for (b, (ty, slot)) in binders.iter().zip(yes_payloads) {
                    self.bind_pattern_var(b, ty, slot, &arm.body);
                }
            }
            let terminated = self.emit_block_stmts(&arm.body);
            if !terminated {
                self.free_scope_frame();
            }
            self.scopes.pop();
        } else if let Some(arm) = wild {
            self.emit_block(&arm.body, false);
        }
        self.indent -= 1;
        self.line("} else {");
        self.indent += 1;
        if let Some(arm) = no {
            self.scopes.push(Scope { owned: Vec::new(), is_loop: false });
            if let IrPattern::Variant { binders, .. } = &arm.pattern {
                for (b, (ty, slot)) in binders.iter().zip(no_payloads) {
                    self.bind_pattern_var(b, ty, slot, &arm.body);
                }
            }
            let terminated = self.emit_block_stmts(&arm.body);
            if !terminated {
                self.free_scope_frame();
            }
            self.scopes.pop();
        } else if let Some(arm) = wild {
            self.emit_block(&arm.body, false);
        }
        self.indent -= 1;
        self.line("}");
    }

    /// Match binders own copies of the payload (mirrors the Python backend).
    fn bind_pattern_var(&mut self, name: &str, ty: &Ty, slot: &str, body: &[IrStmt]) {
        if managed(ty) {
            self.line(&format!("{} {name} = {};", c_type(ty), copy_of(ty, &format!("&{slot}"))));
            self.scopes.last_mut().unwrap().owned.push((name.to_string(), ty.clone()));
        } else {
            self.line(&format!("{} {name} = {slot};", c_type(ty)));
        }
        if !uses_local(body, name) {
            self.line(&format!("(void){name};"));
        }
    }

    fn emit_return(&mut self, v: Option<&IrExpr>) {
        match v {
            None => {
                self.flush_stmt_temps();
                self.free_all_scopes();
                self.line("return;");
            }
            Some(e) => {
                let code = self.store(e);
                let tmp = self.fresh();
                self.line(&format!("{} {tmp} = {code};", c_type(&e.ty)));
                self.flush_stmt_temps();
                self.free_all_scopes();
                self.line(&format!("return {tmp};"));
            }
        }
    }

    fn free_all_scopes(&mut self) {
        let frees: Vec<String> = self
            .scopes
            .iter()
            .rev()
            .flat_map(|s| s.owned.iter().rev())
            .map(|(n, t)| self.free_stmt_for(n, t))
            .collect();
        for f in frees {
            self.line(&f);
        }
    }

    // ---- places -----------------------------------------------------------

    fn place_lvalue(&mut self, p: &Place) -> String {
        match p {
            Place::Var(n) => self.local_lvalue(n),
            Place::Index { base, base_ty, index } => {
                let b = self.place_lvalue(base);
                match base_ty {
                    Ty::List(_) => {
                        let i = self.eval(index);
                        format!("(*{}_at({}, {}))", mangle(base_ty), addr_of(&b), i.code)
                    }
                    Ty::Map(k, _) => {
                        let kp = self.key_ptr(index, k);
                        format!("(*{}_at({}, {kp}))", mangle(base_ty), addr_of(&b))
                    }
                    _ => unreachable!(),
                }
            }
            Place::Field { base, name, .. } => {
                let b = self.place_lvalue(base);
                format!("{b}.{name}")
            }
        }
    }

    /// Pointer expression for an already-evaluated value.
    fn val_ptr(&mut self, v: &CVal, ty: &Ty) -> String {
        match v.kind {
            ValKind::Scalar => format!("&({}){{{}}}", c_type(ty), v.code),
            _ => format!("&{}", v.code),
        }
    }

    /// A `const K *` expression for a key value.
    fn key_ptr(&mut self, key: &IrExpr, k_ty: &Ty) -> String {
        let v = self.eval(key);
        match v.kind {
            ValKind::Scalar => format!("&({}){{{}}}", c_type(k_ty), v.code),
            ValKind::Borrow | ValKind::Owned => format!("&{}", v.code),
        }
    }

    // ---- expressions ------------------------------------------------------

    /// Owned-value expression for a storing position; marks moved temps.
    fn store(&mut self, e: &IrExpr) -> String {
        let v = self.eval(e);
        self.store_val(v)
    }

    fn store_val(&mut self, v: CVal) -> String {
        match v.kind {
            ValKind::Scalar => v.code,
            ValKind::Borrow => copy_of(&v.ty, &format!("&{}", v.code)),
            ValKind::Owned => {
                if let Some(pos) = self.stmt_temps.iter().position(|(n, _)| *n == v.code) {
                    self.stmt_temps.remove(pos); // moved
                }
                v.code
            }
        }
    }

    /// Store, coercing the type (used for map keys where the checker already
    /// guaranteed the types line up).
    fn store_expr_of(&mut self, e: &IrExpr, _ty: &Ty) -> String {
        self.store(e)
    }

    /// Pointer to a readable value (for eq / hash / receiver positions).
    fn read_ptr(&mut self, e: &IrExpr) -> String {
        let v = self.eval(e);
        match v.kind {
            ValKind::Scalar => format!("&({}){{{}}}", c_type(&e.ty), v.code),
            _ => addr_of(&v.code),
        }
    }

    fn owned_temp(&mut self, ty: &Ty, code: &str) -> CVal {
        let tmp = self.fresh();
        self.line(&format!("{} {tmp} = {code};", c_type(ty)));
        if managed(ty) {
            self.stmt_temps.push((tmp.clone(), ty.clone()));
            CVal { code: tmp, kind: ValKind::Owned, ty: ty.clone() }
        } else {
            CVal { code: tmp, kind: ValKind::Scalar, ty: ty.clone() }
        }
    }

    pub(crate) fn eval(&mut self, e: &IrExpr) -> CVal {
        let scalar = |code: String, ty: &Ty| CVal { code, kind: ValKind::Scalar, ty: ty.clone() };
        match &e.kind {
            IrExprKind::Int(v) => scalar(int_lit(*v), &e.ty),
            IrExprKind::Float(v) => {
                let s = format!("{v:?}");
                scalar(if *v < 0.0 { format!("({s})") } else { s }, &e.ty)
            }
            IrExprKind::Bool(v) => scalar(if *v { "true" } else { "false" }.into(), &e.ty),
            IrExprKind::Text(s) => {
                if s.is_empty() {
                    let code = format!("{}_new()", mangle(&Ty::list(Ty::Int)));
                    self.owned_temp(&e.ty, &code)
                } else {
                    let items: Vec<String> = s.iter().map(|v| int_lit(*v)).collect();
                    let code = format!(
                        "{}_from((const int64_t[]){{{}}}, {})",
                        mangle(&Ty::list(Ty::Int)),
                        items.join(", "),
                        s.len()
                    );
                    self.owned_temp(&e.ty, &code)
                }
            }
            IrExprKind::Local(n) => {
                let code = self.local_lvalue(n);
                if managed(&e.ty) {
                    CVal { code, kind: ValKind::Borrow, ty: e.ty.clone() }
                } else {
                    scalar(code, &e.ty)
                }
            }
            IrExprKind::Const(n) => scalar(n.clone(), &e.ty),
            IrExprKind::FuncRef(n) => scalar(n.clone(), &e.ty),
            IrExprKind::List(xs) => self.eval_list_lit(&e.ty, xs),
            IrExprKind::Tuple(xs) => {
                let vals: Vec<String> = xs.iter().map(|x| self.store(x)).collect();
                let fields: Vec<String> =
                    vals.iter().enumerate().map(|(i, v)| format!(".f{i} = {v}")).collect();
                let code = format!("({}){{ {} }}", c_type(&e.ty), fields.join(", "));
                self.owned_temp(&e.ty, &code)
            }
            IrExprKind::CallFunc { name, args } => self.eval_call(name, args, &e.ty),
            IrExprKind::CallValue { callee, args } => {
                let c = self.eval(callee);
                let a: Vec<String> = args.iter().map(|x| self.arg_val(x)).collect();
                let code = format!("{}({})", c.code, a.join(", "));
                if e.ty == Ty::Tuple(Vec::new()) {
                    self.line(&format!("{code};"));
                    CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: e.ty.clone() }
                } else {
                    self.owned_temp(&e.ty, &code)
                }
            }
            IrExprKind::NewRecord { name, args } => {
                let r = self.m.record(name).expect("record exists").clone();
                let vals: Vec<String> = args.iter().map(|x| self.store(x)).collect();
                let fields: Vec<String> = r
                    .fields
                    .iter()
                    .zip(&vals)
                    .map(|((f, _), v)| format!(".{f} = {v}"))
                    .collect();
                let code = format!("({name}){{ {} }}", fields.join(", "));
                self.owned_temp(&e.ty, &code)
            }
            IrExprKind::NewVariant { enum_name, variant, args } => {
                let vals: Vec<String> = args.iter().map(|x| self.store(x)).collect();
                let code = match (enum_name.as_str(), variant.as_str()) {
                    ("Option", "Some") => {
                        format!("{}_some({})", mangle(&e.ty), vals.join(", "))
                    }
                    ("Option", "None") => format!("{}_none()", mangle(&e.ty)),
                    ("Result", "Ok") => format!("{}_ok({})", mangle(&e.ty), vals.join(", ")),
                    ("Result", "Err") => {
                        format!("{}_err({})", mangle(&e.ty), vals.join(", "))
                    }
                    _ => format!(
                        "{}({})",
                        sudoc_ir::mangle::variant_class(enum_name, variant),
                        vals.join(", ")
                    ),
                };
                self.owned_temp(&e.ty, &code)
            }
            IrExprKind::Builtin { builtin, args } => self.eval_builtin(*builtin, args, &e.ty),
            IrExprKind::MutBuiltin { builtin, recv, recv_ty, args } => {
                self.eval_mut_builtin(*builtin, recv, recv_ty, args, &e.ty)
            }
            IrExprKind::GetField { recv, name } => {
                let r = self.eval(recv);
                let code = format!("{}.{name}", r.code);
                if managed(&e.ty) {
                    CVal { code, kind: ValKind::Borrow, ty: e.ty.clone() }
                } else {
                    scalar(code, &e.ty)
                }
            }
            IrExprKind::Index { recv, index } => {
                let r = self.eval(recv);
                match &recv.ty {
                    Ty::List(_) => {
                        let i = self.eval(index);
                        let code =
                            format!("(*{}_at({}, {}))", mangle(&recv.ty), addr_of(&r.code), i.code);
                        if managed(&e.ty) {
                            CVal { code, kind: ValKind::Borrow, ty: e.ty.clone() }
                        } else {
                            // Trapping scalar: materialize for order.
                            self.owned_temp(&e.ty, &code)
                        }
                    }
                    Ty::Map(k, _) => {
                        let kp = self.key_ptr(index, k);
                        let code = format!("(*{}_at({}, {kp}))", mangle(&recv.ty), addr_of(&r.code));
                        if managed(&e.ty) {
                            CVal { code, kind: ValKind::Borrow, ty: e.ty.clone() }
                        } else {
                            self.owned_temp(&e.ty, &code)
                        }
                    }
                    _ => unreachable!(),
                }
            }
            IrExprKind::Unary { op, operand } => {
                let x = self.eval(operand);
                let code = match (op, &operand.ty) {
                    (UnaryOp::Neg, Ty::Int) => format!("sudo_neg({})", x.code),
                    (UnaryOp::Neg, _) => format!("(-{})", x.code),
                    (UnaryOp::Not, _) => format!("(!{})", x.code),
                };
                scalar(code, &e.ty)
            }
            IrExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, &e.ty),
        }
    }

    fn eval_list_lit(&mut self, ty: &Ty, xs: &[IrExpr]) -> CVal {
        let n = mangle(ty);
        let elem = match ty {
            Ty::List(e) => (**e).clone(),
            _ => unreachable!(),
        };
        if xs.is_empty() {
            return self.owned_temp(ty, &format!("{n}_new()"));
        }
        // Compact form for simple int vectors (test data, text-adjacent).
        let simple = matches!(elem, Ty::Int)
            && xs.iter().all(|x| {
                matches!(x.kind, IrExprKind::Int(_) | IrExprKind::Local(_))
            });
        if simple {
            let items: Vec<String> = xs
                .iter()
                .map(|x| self.eval(x).code)
                .collect();
            let code = format!(
                "{n}_from((const int64_t[]){{{}}}, {})",
                items.join(", "),
                xs.len()
            );
            return self.owned_temp(ty, &code);
        }
        let tmp = self.fresh();
        self.line(&format!("{} {tmp} = {n}_new();", c_type(ty)));
        self.stmt_temps.push((tmp.clone(), ty.clone()));
        for x in xs {
            let v = self.store(x);
            self.line(&format!("{n}_push(&{tmp}, {v});"));
        }
        CVal { code: tmp, kind: ValKind::Owned, ty: ty.clone() }
    }

    /// Argument for a non-inout parameter: an owned value.
    fn arg_val(&mut self, e: &IrExpr) -> String {
        self.store(e)
    }

    fn eval_call(&mut self, name: &str, args: &[IrExpr], ret: &Ty) -> CVal {
        let f = self.m.func(name).expect("callee exists").clone();
        let mut a = Vec::new();
        for (arg, p) in args.iter().zip(&f.params) {
            if p.inout {
                // The checker guarantees this is a Var or Field path.
                let place = expr_as_place(arg);
                let lv = self.place_lvalue(&place);
                a.push(addr_of(&lv));
            } else {
                a.push(self.arg_val(arg));
            }
        }
        let code = format!("{name}({})", a.join(", "));
        if f.ret.is_none() {
            self.line(&format!("{code};"));
            return CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: ret.clone() };
        }
        self.owned_temp(ret, &code)
    }

    fn eval_builtin(&mut self, b: Builtin, args: &[IrExpr], ret: &Ty) -> CVal {
        let scalar = |code: String, ty: &Ty| CVal { code, kind: ValKind::Scalar, ty: ty.clone() };
        match b {
            Builtin::AbsInt => {
                let x = self.eval(&args[0]);
                scalar(format!("sudo_abs({})", x.code), ret)
            }
            Builtin::AbsFloat => {
                let x = self.eval(&args[0]);
                scalar(format!("fabs({})", x.code), ret)
            }
            Builtin::MinInt | Builtin::MaxInt | Builtin::MinFloat | Builtin::MaxFloat => {
                let f = match b {
                    Builtin::MinInt => "sudo_min_i64",
                    Builtin::MaxInt => "sudo_max_i64",
                    Builtin::MinFloat => "sudo_fmin",
                    _ => "sudo_fmax",
                };
                let x = self.eval(&args[0]);
                let y = self.eval(&args[1]);
                scalar(format!("{f}({}, {})", x.code, y.code), ret)
            }
            Builtin::FloatOfInt => {
                let x = self.eval(&args[0]);
                scalar(format!("(double)({})", x.code), ret)
            }
            Builtin::IntOfFloat => {
                let x = self.eval(&args[0]);
                self.owned_temp(ret, &format!("sudo_int_of({})", x.code))
            }
            Builtin::Floor | Builtin::Ceil | Builtin::Round | Builtin::Sqrt => {
                let f = match b {
                    Builtin::Floor => "floor",
                    Builtin::Ceil => "ceil",
                    Builtin::Round => "sudo_round",
                    _ => "sqrt",
                };
                let x = self.eval(&args[0]);
                scalar(format!("{f}({})", x.code), ret)
            }
            Builtin::Filled => {
                let n = self.eval(&args[0]);
                let v = self.store(&args[1]);
                self.owned_temp(ret, &format!("{}_filled({}, {v})", mangle(ret), n.code))
            }
            Builtin::NewMap | Builtin::NewSet => {
                self.owned_temp(ret, &format!("{}_new()", mangle(ret)))
            }
            Builtin::ListLength => {
                let r = self.eval(&args[0]);
                scalar(format!("{}.len", r.code), ret)
            }
            Builtin::MapSize | Builtin::SetSize => {
                let r = self.eval(&args[0]);
                scalar(format!("{}.size", r.code), ret)
            }
            Builtin::MapGet => {
                let recv_ty = args[0].ty.clone();
                let r = self.eval(&args[0]);
                let k_ty = match &recv_ty {
                    Ty::Map(k, _) => (**k).clone(),
                    _ => unreachable!(),
                };
                let kp = self.key_ptr(&args[1], &k_ty);
                self.owned_temp(ret, &format!("{}_get({}, {kp})", mangle(&recv_ty), addr_of(&r.code)))
            }
            Builtin::MapHas | Builtin::SetHas => {
                let recv_ty = args[0].ty.clone();
                let r = self.eval(&args[0]);
                let inner = match &recv_ty {
                    Ty::Map(k, _) => (**k).clone(),
                    Ty::Set(t) => (**t).clone(),
                    _ => unreachable!(),
                };
                let kp = self.key_ptr(&args[1], &inner);
                scalar(format!("{}_has({}, {kp})", mangle(&recv_ty), addr_of(&r.code)), ret)
            }
            Builtin::MapKeys | Builtin::MapValues | Builtin::SetItems => {
                let recv_ty = args[0].ty.clone();
                let r = self.eval(&args[0]);
                let f = match b {
                    Builtin::MapKeys => "keys",
                    Builtin::MapValues => "values",
                    _ => "items",
                };
                self.owned_temp(ret, &format!("{}_{f}({})", mangle(&recv_ty), addr_of(&r.code)))
            }
            Builtin::OptIsSome | Builtin::OptIsNone => {
                let r = self.eval(&args[0]);
                let neg = if b == Builtin::OptIsNone { "!" } else { "" };
                scalar(format!("{neg}({}).has", r.code), ret)
            }
            Builtin::ResIsOk | Builtin::ResIsErr => {
                let r = self.eval(&args[0]);
                let neg = if b == Builtin::ResIsErr { "!" } else { "" };
                scalar(format!("{neg}({}).is_ok", r.code), ret)
            }
            Builtin::OptUnwrap | Builtin::ResUnwrap => {
                let recv_ty = args[0].ty.clone();
                let r = self.eval(&args[0]);
                self.owned_temp(ret, &format!("{}_unwrap({})", mangle(&recv_ty), addr_of(&r.code)))
            }
            Builtin::OptGetOr | Builtin::ResGetOr => {
                let recv_ty = args[0].ty.clone();
                let r = self.eval(&args[0]);
                let d = self.store(&args[1]);
                self.owned_temp(ret, &format!("{}_get_or({}, {d})", mangle(&recv_ty), addr_of(&r.code)))
            }
            _ => unreachable!("mutating builtin in non-mut position: {b:?}"),
        }
    }

    fn eval_mut_builtin(
        &mut self,
        b: Builtin,
        recv: &Place,
        recv_ty: &Ty,
        args: &[IrExpr],
        ret: &Ty,
    ) -> CVal {
        let n = mangle(recv_ty);
        let lv = self.place_lvalue(recv);
        match b {
            Builtin::ListAppend => {
                let v = self.store(&args[0]);
                self.line(&format!("{n}_push({}, {v});", addr_of(&lv)));
                CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: ret.clone() }
            }
            Builtin::ListPop => self.owned_temp(ret, &format!("{n}_pop({})", addr_of(&lv))),
            Builtin::ListInsert => {
                let i = self.eval(&args[0]);
                let v = self.store(&args[1]);
                self.line(&format!("{n}_insert({}, {}, {v});", addr_of(&lv), i.code));
                CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: ret.clone() }
            }
            Builtin::ListRemoveAt => {
                let i = self.eval(&args[0]);
                self.owned_temp(ret, &format!("{n}_remove_at({}, {})", addr_of(&lv), i.code))
            }
            Builtin::ListSwap => {
                let i = self.eval(&args[0]);
                let j = self.eval(&args[1]);
                self.line(&format!("{n}_swap({}, {}, {});", addr_of(&lv), i.code, j.code));
                CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: ret.clone() }
            }
            Builtin::ListSort => {
                self.line(&format!("{n}_sort({});", addr_of(&lv)));
                CVal { code: "/*void*/".into(), kind: ValKind::Scalar, ty: ret.clone() }
            }
            Builtin::MapDelete => {
                let k_ty = match recv_ty {
                    Ty::Map(k, _) => (**k).clone(),
                    _ => unreachable!(),
                };
                let kp = self.key_ptr(&args[0], &k_ty);
                self.owned_temp(ret, &format!("{n}_delete({}, {kp})", addr_of(&lv)))
            }
            Builtin::SetAdd => {
                let v = self.store(&args[0]);
                self.owned_temp(ret, &format!("{n}_add({}, {v})", addr_of(&lv)))
            }
            Builtin::SetRemove => {
                let t_ty = match recv_ty {
                    Ty::Set(t) => (**t).clone(),
                    _ => unreachable!(),
                };
                let kp = self.key_ptr(&args[0], &t_ty);
                self.owned_temp(ret, &format!("{n}_remove({}, {kp})", addr_of(&lv)))
            }
            _ => unreachable!("non-mutating builtin in mut position: {b:?}"),
        }
    }

    fn eval_binary(&mut self, op: BinaryOp, lhs: &IrExpr, rhs: &IrExpr, ret: &Ty) -> CVal {
        let scalar = |code: String, ty: &Ty| CVal { code, kind: ValKind::Scalar, ty: ty.clone() };
        match op {
            BinaryOp::And | BinaryOp::Or => {
                if can_trap(rhs) {
                    // Keep the right side lazy: statement-lower.
                    let l = self.eval(lhs);
                    let tmp = self.fresh();
                    self.line(&format!("bool {tmp} = {};", l.code));
                    let guard =
                        if op == BinaryOp::And { tmp.clone() } else { format!("!{tmp}") };
                    self.line(&format!("if ({}) {{", trim_parens(&guard)));
                    self.indent += 1;
                    let before = self.stmt_temps.len();
                    let r = self.eval(rhs);
                    self.line(&format!("{tmp} = {};", r.code));
                    let temps: Vec<(String, Ty)> =
                        self.stmt_temps.drain(before..).rev().collect();
                    for (n, t) in temps {
                        self.line(&format!("{}_free(&{n});", mangle(&t)));
                    }
                    self.indent -= 1;
                    self.line("}");
                    scalar(tmp, ret)
                } else {
                    let l = self.eval(lhs);
                    let r = self.eval(rhs);
                    let sym = if op == BinaryOp::And { "&&" } else { "||" };
                    scalar(format!("({} {sym} {})", l.code, r.code), ret)
                }
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => match &lhs.ty {
                Ty::Int => {
                    let f = match op {
                        BinaryOp::Add => "sudo_add",
                        BinaryOp::Sub => "sudo_sub",
                        _ => "sudo_mul",
                    };
                    let l = self.eval(lhs);
                    let r = self.eval(rhs);
                    scalar(format!("{f}({}, {})", l.code, r.code), ret)
                }
                Ty::Float => {
                    let sym = match op {
                        BinaryOp::Add => "+",
                        BinaryOp::Sub => "-",
                        _ => "*",
                    };
                    let l = self.eval(lhs);
                    let r = self.eval(rhs);
                    scalar(format!("({} {sym} {})", l.code, r.code), ret)
                }
                Ty::List(_) => {
                    let lp = self.read_ptr(lhs);
                    let rp = self.read_ptr(rhs);
                    let code = format!("{}_concat({lp}, {rp})", mangle(&lhs.ty));
                    self.owned_temp(ret, &code)
                }
                _ => unreachable!(),
            },
            BinaryOp::Div => {
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                if matches!(lhs.ty, Ty::Int) {
                    self.owned_temp(ret, &format!("sudo_div({}, {})", l.code, r.code))
                } else {
                    scalar(format!("({} / {})", l.code, r.code), ret)
                }
            }
            BinaryOp::Mod => {
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                self.owned_temp(ret, &format!("sudo_mod({}, {})", l.code, r.code))
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let sym = match op {
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    _ => ">=",
                };
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                scalar(format!("({} {sym} {})", l.code, r.code), ret)
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                if is_scalar(&lhs.ty) {
                    let sym = if op == BinaryOp::Eq { "==" } else { "!=" };
                    let l = self.eval(lhs);
                    let r = self.eval(rhs);
                    scalar(format!("({} {sym} {})", l.code, r.code), ret)
                } else {
                    let lp = self.read_ptr(lhs);
                    let rp = self.read_ptr(rhs);
                    let eq = eq_of(&lhs.ty, &lp, &rp);
                    let code = if op == BinaryOp::Eq { eq } else { format!("(!{eq})") };
                    scalar(code, ret)
                }
            }
        }
    }
}

/// Inout arguments are guaranteed (by the checker) to be Var/Field paths.
fn expr_as_place(e: &IrExpr) -> Place {
    match &e.kind {
        IrExprKind::Local(n) => Place::Var(n.clone()),
        IrExprKind::GetField { recv, name } => Place::Field {
            base: Box::new(expr_as_place(recv)),
            base_ty: recv.ty.clone(),
            name: name.clone(),
        },
        other => unreachable!("inout arg is not a place: {other:?}"),
    }
}
