//! JavaScript backend: typed IR -> readable ES-module JavaScript (Node 18+).
//!
//! Value semantics strategy (lockstep.md §5.1), mirroring backend_py:
//! - every mutable composite is copy-on-write (`CowList` / map / set /
//!   `CowRec`); tuples stay plain arrays. `_rt.dup` is an O(1) share; a
//!   write forks that object if it has a second referent. Nested
//!   mutation goes through `at_mut` / `field_mut`;
//! - a record type whose fields no body writes in place
//!   ([`field_written_records`]) is a plain object shared by reference: no
//!   `CowRec`, and no `dup` where it is stored;
//! - a read-only tuple destructure (`a, s = ys[0]` where no binding is
//!   written) skips `dup`; a written binding still copies;
//! - a permutation of distinct locals (`items, buf = buf, items`) is a
//!   rebinding — no store/dup (the merge-sort ping-pong);
//! - non-inout composite parameters are defensively copied at function entry;
//! - aliasing reads (variables, fields, indexing, unwraps) are copied at
//!   storing positions (assignment RHS, constructor args, container inserts,
//!   returns); enums and Option/Result share safely because nothing can
//!   mutate them in place — their payloads are copied on extraction instead;
//! - inout parameters are threaded through return values: every function
//!   returns `[ret?, ...inouts]` (bare value when a single component) and
//!   call sites reassign — `n = bump(n)` / `[y, n] = take(n)`;
//! - deep equality goes through `_rt.eq` (arrays have no built-in deep equal);
//! - `sudo int` is BigInt (`5n`); `sudo float` is IEEE number. A small
//!   straight-line helper is inlined at the call, and the integer stores in
//!   that copy are JS numbers when `Number.isSafeInteger` holds, otherwise
//!   the same `_rt.chk` BigInt operation. A uniquely owned list rebuild
//!   (`rc == 1`) updates the list in place;
//! - match lowers to `if/else if` with `instanceof` so loop `break`/`continue`
//!   inside match arms target the enclosing loop with no switch capture.

use sudoc_ir::never_written::{
    expr_root_var, field_written_records, inout_roots, written_in_stmts, written_locals,
};
use sudoc_ir::ownership::{self, Ownership};
use sudoc_ir::{
    BinaryOp, Builtin, IrExpr, IrExprKind, IrFunc, IrModule, IrPattern, IrStmt, Place, Ty, UnaryOp,
};

mod boundary;
mod inline;
mod reserved;
pub use boundary::{api_file, emit_api};

/// The shared JavaScript runtime, written alongside every generated module.
pub const RUNTIME: &str = include_str!("runtime/_sudo_rt.mjs");
pub const RUNTIME_FILE: &str = "_sudo_rt.mjs";

/// File name of the internal implementation module.
pub fn impl_file(module: &str) -> String {
    format!("_{module}_impl.mjs")
}

/// Emit a JS module for the IR. When `with_tests` is set, `test` blocks
/// become `test_*` functions plus a direct-run runner that executes them.
pub fn emit(module: &IrModule, with_tests: bool) -> String {
    let module = reserved::rename_reserved(module);
    let all = std::slice::from_ref(&module);
    emit_with(&module, all, &shared_records(all), with_tests, with_tests)
}

/// Record types shared by reference: no body in `all` writes their fields.
fn shared_records(all: &[IrModule]) -> std::collections::HashSet<String> {
    let field_written = field_written_records(all);
    all.iter()
        .flat_map(|m| &m.records)
        .filter(|r| !field_written.contains(&r.name))
        .map(|r| r.name.clone())
        .collect()
}

/// Emit `module`, resolving cross-module callee signatures (inout-taking
/// functions in particular) against `all_modules` — the whole program,
/// not just `module` itself. `emit_program` calls this with the real
/// program; `emit` (single-module callers, back-compat) passes a
/// one-element slice. `shared` is [`shared_records`] of `all_modules`;
/// `freeze` (a `--tests` build) freezes values of those types.
fn emit_with(
    module: &IrModule,
    all_modules: &[IrModule],
    shared: &std::collections::HashSet<String>,
    with_tests: bool,
    freeze: bool,
) -> String {
    let plan = inline::plan(module);
    Emitter {
        m: module,
        all: all_modules,
        out: String::new(),
        written: std::collections::HashSet::new(),
        shared,
        freeze,
        own: Ownership::default(),
        plan,
        cur_fn: String::new(),
        lower: false,
        ret_slot: None,
        done_flag: None,
        seq: 0,
        fast: None,
        direct: None,
        capture_loop: false,
        divert_hoist: false,
        invariant: std::collections::HashSet::new(),
        hoist_buf: String::new(),
        loop_buf: String::new(),
        inline_stack: Vec::new(),
        force_i64: false,
        defining_inlineable: false,
    }
    .run(with_tests)
}

struct FastPath {
    src: String,
    index: String,
    acc: String,
    elem: String,
}

#[derive(Clone)]
struct DirectIndex {
    src: String,
    index: String,
    arr: String,
    ix: String,
}

#[derive(Clone)]
enum RetSlot {
    Name(String),
    Tuple(Vec<String>),
    /// Write a returned record's fields onto this object.
    InPlace(String),
}

struct Emitter<'a> {
    m: &'a IrModule,
    /// Every module in the program, for cross-module callee lookup.
    all: &'a [IrModule],
    out: String,
    /// Locals written by the function (or test) currently being emitted.
    written: std::collections::HashSet<String>,
    /// Record types shared by reference: no field is ever written in place.
    shared: &'a std::collections::HashSet<String>,
    /// Freeze shared records (`--tests`).
    freeze: bool,
    own: Ownership,
    plan: inline::Plan,
    cur_fn: String,
    /// Inside an inlined helper: integer stores use the number fast path.
    lower: bool,
    ret_slot: Option<RetSlot>,
    /// Per-inline flag so a `return` skips the rest of that copy only.
    done_flag: Option<String>,
    seq: u32,
    /// In-place rebuild loop currently being emitted.
    fast: Option<FastPath>,
    /// `for i = 0 to xs.length - 1` over a list, indexed through `_box.d`.
    direct: Option<DirectIndex>,
    /// Fast-loop body is being split into once-per-call and per-element parts.
    capture_loop: bool,
    divert_hoist: bool,
    /// Locals that do not change between elements of the fast loop.
    invariant: std::collections::HashSet<String>,
    hoist_buf: String,
    loop_buf: String,
    inline_stack: Vec<String>,
    /// Fallback arithmetic: leaves may be numbers and must become BigInt.
    force_i64: bool,
    /// Emitting the body of a helper that is itself inlineable. Its own
    /// calls stay calls so the helper stays small; callers still inline it.
    defining_inlineable: bool,
}

impl Emitter<'_> {
    /// Resolve a callee's signature across the whole program: a bare name
    /// looks in the current module; a `module.func` qualified name looks up
    /// the named module in `self.all`. Mirrors the Haskell backend's
    /// `lookupFunc`/`splitQual` (backends/haskell/Emit.hs).
    fn resolve_func(&self, name: &str) -> Option<&IrFunc> {
        match name.split_once('.') {
            Some((modname, fname)) => self.all.iter().find(|m| m.name == modname)?.func(fname),
            None => self.m.func(name),
        }
    }

    fn type_qual(&self, name: &str) -> String {
        match sudoc_ir::type_module(self.all, name) {
            Some(home) if home != self.m.name => format!("{home}.{name}"),
            Some(_) => name.to_string(),
            None => panic!("internal error: nominal type '{name}' has no declaring module"),
        }
    }

    fn variant_qual(&self, enum_name: &str, variant: &str) -> String {
        let cls = sudoc_ir::mangle::variant_class(enum_name, variant);
        match sudoc_ir::type_module(self.all, enum_name) {
            Some(home) if home != self.m.name => format!("{home}.{cls}"),
            Some(_) => cls,
            None => panic!("internal error: enum '{enum_name}' has no declaring module"),
        }
    }

    fn enum_def(&self, name: &str) -> &sudoc_ir::IrEnum {
        sudoc_ir::find_enum(self.all, name)
            .unwrap_or_else(|| panic!("internal error: enum '{name}' not found in program"))
    }

    fn run(mut self, with_tests: bool) -> String {
        self.line(
            0,
            &format!(
                "// Generated by sudoc from {}.sudo — do not edit; edit the sudo source.",
                self.m.name
            ),
        );
        self.line(0, "import * as _rt from \"./_sudo_rt.mjs\";");
        for dep in &self.m.imports {
            self.line(0, &format!("import * as {dep} from \"./_{dep}_impl.mjs\";"));
        }
        self.blank();

        for r in &self.m.records {
            self.line(0, &format!("export class {} {{", r.name));
            self.line(1, &format!("static _sudoKind = [\"r\", \"{}\"];", r.name));
            let fields: Vec<&str> = r.fields.iter().map(|f| f.name.as_str()).collect();
            let field_lits: Vec<String> = fields.iter().map(|f| format!("\"{f}\"")).collect();
            self.line(
                1,
                &format!("static _sudoFields = [{}];", field_lits.join(", ")),
            );
            if self.shared.contains(&r.name) {
                self.line(1, "static _sudoShared = true;");
            }
            self.line(1, &format!("constructor({}) {{", fields.join(", ")));
            let numeric = self.plan.mut_recs.contains(&r.name);
            for f in &r.fields {
                if numeric && matches!(f.ty, Ty::Int) {
                    // The hot loops read these fields as JS numbers.
                    self.line(
                        2,
                        &format!(
                            "this.{0} = (typeof {0} === \"bigint\" && {0} >= {SAFE_MIN} && {0} <= {SAFE_MAX}) ? globalThis.Number({0}) : {0};",
                            f.name
                        ),
                    );
                } else {
                    self.line(2, &format!("this.{} = {};", f.name, f.name));
                }
            }
            if self.freeze && self.shared.contains(&r.name) && !numeric {
                // Under --tests, a field write the analysis missed throws
                // instead of silently writing through a shared value.
                // A record a unique list rebuild writes in place stays mutable.
                self.line(2, "Object.freeze(this);");
            }
            self.line(1, "}");
            self.line(0, "}");
            self.blank();
        }
        for e in &self.m.enums {
            for v in &e.variants {
                let cls = sudoc_ir::mangle::variant_class(&e.name, &v.name);
                self.line(0, &format!("export class {cls} {{"));
                self.line(
                    1,
                    &format!("static _sudoKind = [\"e\", \"{}.{}\"];", e.name, v.name),
                );
                let fields: Vec<&str> = v.fields.iter().map(|f| f.name.as_str()).collect();
                let field_lits: Vec<String> = fields.iter().map(|f| format!("\"{f}\"")).collect();
                self.line(
                    1,
                    &format!("static _sudoFields = [{}];", field_lits.join(", ")),
                );
                self.line(1, &format!("constructor({}) {{", fields.join(", ")));
                for f in &fields {
                    self.line(2, &format!("this.{f} = {f};"));
                }
                self.line(1, "}");
                self.line(0, "}");
                self.blank();
            }
        }
        for c in &self.m.consts {
            // store() so top-level Const refs to composite module constants
            // deep-copy (value semantics) rather than aliasing the same object.
            let value = self.store(&c.value, 0);
            self.line(0, &format!("export const {} = {};", c.name, value));
        }
        if !self.m.consts.is_empty() {
            self.blank();
        }

        for f in &self.m.funcs {
            self.func(f);
            self.blank();
        }

        if with_tests {
            let names = sudoc_ir::names::test_fn_names(&self.m.tests);
            for (t, name) in self.m.tests.iter().zip(&names) {
                self.cur_fn.clear();
                self.written = written_in_stmts(&t.body, self.m, self.all);
                self.own = ownership::analyze(&[], &t.body, self.m, self.all);
                self.line(0, &format!("function {name}() {{"));
                if !t.body.is_empty() {
                    self.block(&t.body, 1, &[]);
                }
                self.line(0, "}");
                self.blank();
            }
            // Explicit test list: JS modules have no globals() equivalent.
            // Realpath both sides: argv[1] may be a non-realpath absolute path
            // (e.g. /var/... vs /private/var/... on macOS) while import.meta.url
            // is already realpath-resolved by the ESM loader.
            self.line(0, "import { fileURLToPath } from \"node:url\";");
            self.line(0, "import fs from \"node:fs\";");
            self.line(0, "import path from \"node:path\";");
            self.line(0, "const _sudo_main = (() => {");
            self.line(1, "try {");
            self.line(
                2,
                "return process.argv[1] && fs.realpathSync(path.resolve(process.argv[1])) === fileURLToPath(import.meta.url);",
            );
            self.line(1, "} catch {");
            self.line(2, "return false;");
            self.line(1, "}");
            self.line(0, "})();");
            self.line(0, "if (_sudo_main) {");
            let pairs: Vec<String> = names.iter().map(|n| format!("[\"{n}\", {n}]")).collect();
            self.line(
                1,
                &format!("process.exit(_rt.run_tests([{}]));", pairs.join(", ")),
            );
            self.line(0, "}");
        }
        self.out
    }

    fn func(&mut self, f: &IrFunc) {
        self.cur_fn = f.name.clone();
        self.written = written_locals(f, self.m, self.all);
        self.own = ownership::analyze(&f.params, &f.body, self.m, self.all);
        let params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        self.line(
            0,
            &format!("export function {}({}) {{", f.name, params.join(", ")),
        );
        for p in &f.params {
            if !p.inout && self.needs_dup(&p.ty) && !p.never_written {
                self.line(1, &format!("{0} = _rt.dup({0});", p.name));
            }
        }
        if self.plan.lowered.contains(&f.name) || self.plan.rebuilds.contains_key(&f.name) {
            for p in &f.params {
                self.coerce_param(&p.name, &p.ty, 1);
            }
        }
        let inouts: Vec<String> = f
            .params
            .iter()
            .filter(|p| p.inout)
            .map(|p| p.name.clone())
            .collect();
        if let Some(reb) = self.plan.rebuilds.get(&f.name).cloned() {
            self.emit_fast_rebuild(&reb, 1);
        }
        let saved_def = self.defining_inlineable;
        let saved_lower = self.lower;
        // Expand callees into this body. A list rebuild does not inline, so
        // its loop stays small enough to optimize; the helpers are lowered
        // on their own.
        self.defining_inlineable = false;
        self.lower = self.plan.lowered.contains(&f.name);
        self.block(&f.body, 1, &inouts);
        self.defining_inlineable = saved_def;
        self.lower = saved_lower;
        if !definitely_returns(&f.body) && !inouts.is_empty() {
            self.line(1, &format!("return {};", ret_parts(&inouts)));
        }
        self.line(0, "}");
    }

    fn block(&mut self, stmts: &[IrStmt], depth: usize, inouts: &[String]) {
        if self.ret_slot.is_some() {
            self.block_inlined(stmts, depth, inouts);
            return;
        }
        for s in stmts {
            self.stmt(s, depth, inouts);
        }
    }

    /// Statements after a `return` in this copy are skipped. A chain of
    /// `if c { return }` plus a final return becomes `if / else if / else`,
    /// so the copy does not need a flag. Anything else guards the tail.
    fn block_inlined(&mut self, stmts: &[IrStmt], depth: usize, inouts: &[String]) {
        if let Some(at) = ladder_at(stmts) {
            if at > 0 {
                self.block_inlined(&stmts[..at], depth, inouts);
            }
            self.emit_ladder(&stmts[at..], depth);
            return;
        }
        let mut guard = false;
        for s in stmts {
            let d = if guard { depth + 1 } else { depth };
            if guard {
                let flag = self
                    .done_flag
                    .clone()
                    .unwrap_or_else(|| "_sudo_done".into());
                self.line(depth, &format!("if (!{flag}) {{"));
            }
            self.stmt(s, d, inouts);
            if guard {
                self.line(depth, "}");
            }
            if stmt_may_return(s) {
                guard = true;
            }
        }
    }

    /// `if c { ... return }` else `if` else `return`. Each next condition is
    /// evaluated inside the `else`, so its temps stay in scope and a later
    /// arm does not run when an earlier one returns.
    fn emit_ladder(&mut self, stmts: &[IrStmt], depth: usize) {
        let mut opened = false;
        for (i, s) in stmts.iter().enumerate() {
            let IrStmt::If { arms, .. } = s else {
                if opened {
                    self.line(depth, "} else {");
                    self.emit_return_into_slot(return_expr(s), depth + 1);
                    self.line(depth, "}");
                } else {
                    self.emit_return_into_slot(return_expr(s), depth);
                }
                return;
            };
            let (cond, body) = &arms[0];
            let mark = self.buf_len();
            let c = self.expr(cond, depth);
            if self.buf_len() != mark {
                // The condition spilled temps. They were emitted at this
                // depth; nest the rest so a later arm does not see them.
                let nest = if opened { depth + 1 } else { depth };
                if opened {
                    self.line(depth, "} else {");
                }
                self.line(nest, &format!("if ({c}) {{"));
                self.emit_arm_returning(body, nest + 1);
                self.line(nest, "} else {");
                self.emit_ladder(&stmts[i + 1..], nest + 1);
                self.line(nest, "}");
                if opened {
                    self.line(depth, "}");
                }
                return;
            }
            if opened {
                self.line(depth, &format!("}} else if ({c}) {{"));
            } else {
                self.line(depth, &format!("if ({c}) {{"));
                opened = true;
            }
            self.emit_arm_returning(body, depth + 1);
        }
        if opened {
            self.line(depth, "}");
        }
    }

    fn buf_len(&self) -> usize {
        if self.capture_loop {
            if self.divert_hoist {
                self.hoist_buf.len()
            } else {
                self.loop_buf.len()
            }
        } else {
            self.out.len()
        }
    }

    /// Later arms sit in the `else`, so a condition temp is not declared in
    /// the previous arm and is not evaluated when that arm is taken.
    fn emit_if(
        &mut self,
        arms: &[(IrExpr, Vec<IrStmt>)],
        else_block: Option<&[IrStmt]>,
        depth: usize,
        inouts: &[String],
    ) {
        let Some((cond, body)) = arms.first() else {
            if let Some(body) = else_block {
                self.block(body, depth, inouts);
            }
            return;
        };
        let c = self.expr(cond, depth);
        self.line(depth, &format!("if ({c}) {{"));
        self.block(body, depth + 1, inouts);
        let rest = &arms[1..];
        if rest.is_empty() && else_block.is_none() {
            self.line(depth, "}");
            return;
        }
        self.line(depth, "} else {");
        if rest.is_empty() {
            if let Some(body) = else_block {
                self.block(body, depth + 1, inouts);
            }
        } else {
            self.emit_if(rest, else_block, depth + 1, inouts);
        }
        self.line(depth, "}");
    }

    fn emit_arm_returning(&mut self, body: &[IrStmt], depth: usize) {
        if let Some((IrStmt::Return(_), prefix)) = body.split_last() {
            if !prefix.is_empty() {
                self.block_inlined(prefix, depth, &[]);
            }
            self.emit_return_into_slot(return_expr(body.last().unwrap()), depth);
            return;
        }
        self.block_inlined(body, depth, &[]);
    }

    fn stmt(&mut self, s: &IrStmt, depth: usize, inouts: &[String]) {
        let inv = self.capture_loop && self.stmt_invariant(s);
        if inv {
            self.divert_hoist = true;
        }
        self.stmt_emit(s, depth, inouts);
        if self.capture_loop {
            self.divert_hoist = false;
            self.track_assigned(s, inv);
        }
    }

    /// Pure assignment from loop-invariant inputs. `xp = 0` is not included:
    /// it resets a per-element local and must stay in the loop.
    fn stmt_invariant(&self, s: &IrStmt) -> bool {
        let value = match s {
            IrStmt::Assign { value, .. } | IrStmt::TupleAssign { value, .. } => value,
            _ => return false,
        };
        let mut reads_inv = false;
        let mut reads_var = false;
        self.scan_invariance(value, &mut reads_inv, &mut reads_var);
        reads_inv && !reads_var
    }

    fn scan_invariance(&self, e: &IrExpr, reads_inv: &mut bool, reads_var: &mut bool) {
        match &e.kind {
            IrExprKind::Local(n) => {
                if self.invariant.contains(n) {
                    *reads_inv = true;
                } else {
                    *reads_var = true;
                }
            }
            IrExprKind::Int(_)
            | IrExprKind::Float(_)
            | IrExprKind::Bool(_)
            | IrExprKind::Text(_)
            | IrExprKind::Const(_)
            | IrExprKind::FuncRef(_) => {}
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) => {
                xs.iter()
                    .for_each(|x| self.scan_invariance(x, reads_inv, reads_var));
            }
            IrExprKind::CallFunc { args, .. }
            | IrExprKind::Builtin { args, .. }
            | IrExprKind::NewRecord { args, .. }
            | IrExprKind::NewVariant { args, .. }
            | IrExprKind::MutBuiltin { args, .. } => {
                args.iter()
                    .for_each(|x| self.scan_invariance(x, reads_inv, reads_var));
            }
            IrExprKind::CallValue { callee, args } => {
                self.scan_invariance(callee, reads_inv, reads_var);
                args.iter()
                    .for_each(|x| self.scan_invariance(x, reads_inv, reads_var));
            }
            IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
                self.scan_invariance(recv, reads_inv, reads_var);
            }
            IrExprKind::Index { recv, index } => {
                self.scan_invariance(recv, reads_inv, reads_var);
                self.scan_invariance(index, reads_inv, reads_var);
            }
            IrExprKind::Binary { lhs, rhs, .. } => {
                self.scan_invariance(lhs, reads_inv, reads_var);
                self.scan_invariance(rhs, reads_inv, reads_var);
            }
        }
    }

    fn track_assigned(&mut self, s: &IrStmt, inv: bool) {
        let names: Vec<String> = match s {
            IrStmt::Assign {
                target: Place::Var(n),
                ..
            } => vec![n.clone()],
            IrStmt::TupleAssign { targets, .. } => targets.clone(),
            _ => return,
        };
        for n in names {
            if inv {
                self.invariant.insert(n);
            } else {
                self.invariant.remove(&n);
            }
        }
    }

    fn stmt_emit(&mut self, s: &IrStmt, depth: usize, inouts: &[String]) {
        match s {
            IrStmt::Assign {
                target,
                value,
                declares,
            } => {
                if let Some(call) = self.as_inout_call(value) {
                    self.emit_inout_call(Some(target), call, depth, *declares);
                    return;
                }
                if let IrExprKind::CallFunc { name, args } = &value.kind {
                    if self.want_inline(name) {
                        if let Place::Var(v) = target {
                            if *declares && self.ret_slot.is_none() {
                                self.line(depth, &format!("let {v};"));
                            }
                            if let Some(func) = self.resolve_func(name).cloned() {
                                self.emit_inline(&func, args, RetSlot::Name(v.clone()), depth);
                                return;
                            }
                        }
                    } else {
                        let transfer = args.first().is_some_and(|a| place_matches_arg(target, a));
                        let code = self.emit_call(name, args, transfer, depth);
                        self.assign_to_place(target, &code, depth, *declares);
                        return;
                    }
                }
                // Tuples have no in-place mutation path. Storing one into a
                // container slot can share the object; extraction to a local
                // still dups. This is the merge-loop `buf[k] = items[a]`.
                let v = if dest_can_share_tuple(target) && is_tuple_ty(&value.ty) {
                    self.expr(value, depth)
                } else {
                    self.store(value, depth)
                };
                self.assign_to_place(target, &v, depth, *declares);
            }
            IrStmt::TupleAssign {
                targets,
                declares,
                value,
            } => {
                if let IrExprKind::CallFunc { name, args } = &value.kind {
                    if self.want_inline(name) {
                        if self.ret_slot.is_none() {
                            for (t, d) in targets.iter().zip(declares.iter()) {
                                if *d {
                                    self.line(depth, &format!("let {t};"));
                                }
                            }
                        }
                        if let Some(func) = self.resolve_func(name).cloned() {
                            self.emit_inline(&func, args, RetSlot::Tuple(targets.clone()), depth);
                            return;
                        }
                    }
                }
                // A permutation of distinct locals is a rebinding (the
                // merge-sort `items, buf = buf, items` swap). Emitting
                // store()/dup here would share both lists four times per pass.
                let v = if tuple_is_local_perm(targets, value)
                    || unpack_readonly(targets, &self.written)
                {
                    if tuple_is_local_perm(targets, value) {
                        let IrExprKind::Tuple(xs) = &value.kind else {
                            unreachable!("perm checked")
                        };
                        let items: Vec<String> = xs.iter().map(|x| self.expr(x, depth)).collect();
                        format!("[{}]", items.join(", "))
                    } else {
                        self.expr(value, depth)
                    }
                } else {
                    self.store(value, depth)
                };
                if self.ret_slot.is_none() {
                    for (t, d) in targets.iter().zip(declares.iter()) {
                        if *d {
                            self.line(depth, &format!("let {t};"));
                        }
                    }
                }
                self.line(depth, &format!("[{}] = {};", targets.join(", "), v));
            }
            IrStmt::Expr(e) => {
                if let Some(call) = self.as_inout_call(e) {
                    self.emit_inout_call(None, call, depth, false);
                    return;
                }
                let code = self.expr(e, depth);
                self.line(depth, &format!("{code};"));
            }
            IrStmt::If { arms, else_block } => {
                self.emit_if(arms, else_block.as_deref(), depth, inouts);
            }
            IrStmt::While { cond, body } => {
                let c = self.expr(cond, depth);
                self.line(depth, &format!("while ({c}) {{"));
                self.block(body, depth + 1, inouts);
                self.line(depth, "}");
            }
            IrStmt::ForRange {
                var,
                from,
                to,
                down,
                body,
            } => {
                if !*down {
                    if let Some(src) = list_len_src(from, to) {
                        self.emit_direct_loop(src, var, body, depth, inouts);
                        return;
                    }
                }
                // Bounds evaluated once. BigInt has no wraparound, so a plain
                // increment past i64 MAX terminates normally without trapping.
                let f = self.expr(from, depth);
                let t = self.expr(to, depth);
                let from_tmp = format!("_sudo_from_{var}");
                let to_tmp = format!("_sudo_to_{var}");
                // Block-scope the bound temps (and the loop var) so two `for i`
                // loops in one scope don't redeclare `const _sudo_from_i` — a
                // load-time SyntaxError. Mirrors §5.3 fresh loop-var scoping.
                self.line(depth, "{");
                self.line(depth + 1, &format!("const {from_tmp} = {f};"));
                self.line(depth + 1, &format!("const {to_tmp} = {t};"));
                if *down {
                    self.line(
                        depth + 1,
                        &format!("for (let {var} = {from_tmp}; {var} >= {to_tmp}; {var} -= 1n) {{"),
                    );
                } else {
                    self.line(
                        depth + 1,
                        &format!("for (let {var} = {from_tmp}; {var} <= {to_tmp}; {var} += 1n) {{"),
                    );
                }
                self.block(body, depth + 2, inouts);
                self.line(depth + 1, "}");
                self.line(depth, "}");
            }
            IrStmt::ForIn { vars, iter, body } => {
                let it = self.expr(iter, depth);
                let iterable = match strip(&iter.ty) {
                    Ty::List(_) => format!("{it}.slice()"),
                    Ty::Set(_) => format!("{it}.items_list()"),
                    Ty::Map(..) => format!("{it}.pairs()"),
                    _ => it,
                };
                if vars.len() == 1 {
                    self.line(depth, &format!("for (const {} of {iterable}) {{", vars[0]));
                } else {
                    self.line(
                        depth,
                        &format!("for (const [{}] of {iterable}) {{", vars.join(", ")),
                    );
                }
                self.block(body, depth + 1, inouts);
                self.line(depth, "}");
            }
            IrStmt::Match { scrutinee, arms } => {
                // if/else chain (not switch) so break/continue target loops.
                let sc = self.expr(scrutinee, depth);
                self.line(depth, "{");
                self.line(depth + 1, &format!("const _sudo_sc = {sc};"));
                for (i, arm) in arms.iter().enumerate() {
                    let (cond, binders) = self.pattern_cond_and_binders("_sudo_sc", &arm.pattern);
                    if i == 0 {
                        self.line(depth + 1, &format!("if ({cond}) {{"));
                    } else {
                        self.line(depth + 1, &format!("}} else if ({cond}) {{"));
                    }
                    for (name, access, ty) in binders {
                        if let Some(ty) = ty {
                            if self.needs_dup(&ty) {
                                self.line(depth + 2, &format!("const {name} = _rt.dup({access});"));
                                continue;
                            }
                        }
                        self.line(depth + 2, &format!("const {name} = {access};"));
                    }
                    self.block(&arm.body, depth + 2, inouts);
                }
                if !arms.is_empty() {
                    self.line(depth + 1, "}");
                }
                self.line(depth, "}");
            }
            IrStmt::Return(v) => {
                if self.ret_slot.is_none() {
                    if let Some(e) = v {
                        if let Some(param) = self.mut_param_for_return(e) {
                            if let IrExprKind::NewRecord { name, args } = &e.kind {
                                self.write_rec_fields(name, args, &param, depth);
                                self.line(depth, &format!("return {param};"));
                                return;
                            }
                        }
                    }
                }
                if self.ret_slot.is_some() {
                    self.emit_inline_return(v.as_ref(), depth);
                    return;
                }
                if let Some(e) = v {
                    if let IrExprKind::CallFunc { name, args } = &e.kind {
                        if self.want_inline(name) {
                            let tmp = self.fresh("r");
                            self.line(depth, &format!("let {tmp};"));
                            if let Some(func) = self.resolve_func(name).cloned() {
                                self.emit_inline(&func, args, RetSlot::Name(tmp.clone()), depth);
                            }
                            let mut parts = vec![tmp];
                            parts.extend(inouts.iter().cloned());
                            self.line(depth, &format!("return {};", ret_parts(&parts)));
                            return;
                        }
                        let transfer = args.first().is_some_and(|a| {
                            matches!(&a.kind, IrExprKind::Local(_))
                                && self.callee_steals(name, a)
                                && args
                                    .iter()
                                    .skip(1)
                                    .all(|x| !expr_mentions_local(x, local_name(a).unwrap_or("")))
                        });
                        let code = self.emit_call(name, args, transfer, depth);
                        let mut parts = vec![code];
                        parts.extend(inouts.iter().cloned());
                        self.line(depth, &format!("return {};", ret_parts(&parts)));
                        return;
                    }
                }
                let mut parts: Vec<String> = Vec::new();
                if let Some(v) = v {
                    parts.push(self.store(v, depth));
                }
                parts.extend(inouts.iter().cloned());
                if parts.is_empty() {
                    self.line(depth, "return;");
                } else {
                    self.line(depth, &format!("return {};", ret_parts(&parts)));
                }
            }
            IrStmt::Assert { cond, line } => {
                if let IrExprKind::Binary {
                    op: BinaryOp::Eq,
                    lhs,
                    rhs,
                } = &cond.kind
                {
                    let l = self.expr(lhs, depth);
                    let r = self.expr(rhs, depth);
                    self.line(depth, &format!("_rt.sudo_assert_eq({l}, {r}, {line});"));
                } else {
                    let c = self.expr(cond, depth);
                    self.line(depth, &format!("_rt.sudo_assert({c}, {line});"));
                }
            }
            IrStmt::Skip => {}
            IrStmt::Break => self.line(depth, "break;"),
            IrStmt::Continue => self.line(depth, "continue;"),
            IrStmt::ExpectTrap { kind, body, line } => {
                self.line(depth, "let _trapped = false;");
                self.line(depth, "try {");
                self.block(body, depth + 1, inouts);
                self.line(depth, "} catch (_t) {");
                self.line(depth + 1, "if (!(_t instanceof _rt.SudoTrap)) throw _t;");
                self.line(depth + 1, "_trapped = true;");
                self.line(depth + 1, &format!("if (_t.kind !== \"{kind}\") {{"));
                self.line(
                    depth + 2,
                    &format!(
                        "throw new _rt.SudoTrap(\"AssertFailed\", `line {line}: expected trap {kind}, got ${{_t.kind}}`);"
                    ),
                );
                self.line(depth + 1, "}");
                self.line(depth, "}");
                self.line(depth, "if (!_trapped) {");
                self.line(
                    depth + 1,
                    &format!(
                        "throw new _rt.SudoTrap(\"AssertFailed\", \"line {line}: expected trap {kind}, but nothing trapped\");"
                    ),
                );
                self.line(depth, "}");
            }
        }
    }

    /// Returns (condition expression, binders as (name, access_expr, optional field ty)).
    fn pattern_cond_and_binders(
        &self,
        scrut: &str,
        p: &IrPattern,
    ) -> (String, Vec<(String, String, Option<Ty>)>) {
        match p {
            IrPattern::Int(v) => (format!("{scrut} === {v}n"), Vec::new()),
            IrPattern::Bool(true) => (format!("{scrut} === true"), Vec::new()),
            IrPattern::Bool(false) => (format!("{scrut} === false"), Vec::new()),
            IrPattern::Wildcard => ("true".into(), Vec::new()),
            IrPattern::Variant {
                enum_name,
                variant,
                binders,
            } => {
                let (cls, field_names) = match (enum_name.as_str(), variant.as_str()) {
                    ("Option", "Some") => ("_rt.Some".to_string(), vec!["value".to_string()]),
                    ("Option", "None") => ("_rt.NoneOpt".to_string(), Vec::new()),
                    ("Result", "Ok") => ("_rt.Ok".to_string(), vec!["value".to_string()]),
                    ("Result", "Err") => ("_rt.Err".to_string(), vec!["error".to_string()]),
                    _ => {
                        let cls = self.variant_qual(enum_name, variant);
                        let e = self.enum_def(enum_name);
                        let fields = e
                            .variants
                            .iter()
                            .find(|v| v.name == *variant)
                            .unwrap_or_else(|| {
                                panic!(
                                    "internal error: variant '{variant}' not on enum '{enum_name}'"
                                )
                            })
                            .fields
                            .iter()
                            .map(|f| f.name.clone())
                            .collect();
                        (cls, fields)
                    }
                };
                let cond = format!("{scrut} instanceof {cls}");
                let mut bound = Vec::new();
                for (i, b) in binders.iter().enumerate() {
                    let field = field_names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("_{i}"));
                    let ty = self.variant_field_ty(enum_name, variant, i);
                    bound.push((b.clone(), format!("{scrut}.{field}"), ty));
                }
                (cond, bound)
            }
        }
    }

    fn as_inout_call<'e>(&self, e: &'e IrExpr) -> Option<(&'e str, &'e [IrExpr])> {
        if let IrExprKind::CallFunc { name, args } = &e.kind {
            let f = self.resolve_func(name)?;
            if f.params.iter().any(|p| p.inout) {
                return Some((name, args));
            }
        }
        None
    }

    fn emit_inout_call(
        &mut self,
        target: Option<&Place>,
        (name, args): (&str, &[IrExpr]),
        depth: usize,
        declares: bool,
    ) {
        let f = self.resolve_func(name).expect("callee exists").clone();
        // Guard 2: same root local as both inout and never_written by-value —
        // entry-dup was elided for the by-value param, so snapshot it here.
        let inout_roots = inout_roots(args, &f.params);
        let mut arg_code = Vec::new();
        let mut writebacks = Vec::new();
        for (a, p) in args.iter().zip(&f.params) {
            // Inout of a record-field path must go through field_mut so a
            // shared parent forks before the callee writes. Writeback LHS
            // stays the assignable place.
            let mut code = if p.inout && matches!(&a.kind, IrExprKind::GetField { .. }) {
                self.extract_mut(a, depth)
            } else {
                self.expr(a, depth)
            };
            if !p.inout && p.never_written && self.needs_dup(&a.ty) {
                let shares_inout_root = expr_root_var(a)
                    .map(|r| inout_roots.contains(r))
                    .unwrap_or(false);
                if shares_inout_root {
                    code = format!("_rt.dup({code})");
                }
            }
            if p.inout {
                writebacks.push(self.expr(a, depth));
            }
            arg_code.push(code);
        }
        let call = format!("{name}({})", arg_code.join(", "));
        let mut targets: Vec<String> = Vec::new();
        let mut indirect: Option<&Place> = None;
        let mut ret_declares = false;
        if f.ret.is_some() {
            match target {
                Some(Place::Var(v)) => {
                    targets.push(v.clone());
                    ret_declares = declares;
                }
                Some(p) => {
                    targets.push("_sudo_r".into());
                    indirect = Some(p);
                }
                None => targets.push(String::new()),
            }
        }
        targets.extend(writebacks);
        let lhs = if targets.len() == 1 {
            targets[0].clone()
        } else {
            format!("[{}]", targets.join(", "))
        };
        if targets.len() == 1 && ret_declares {
            self.line(depth, &format!("let {lhs} = {call};"));
        } else if targets.len() == 1 && f.ret.is_some() && matches!(target, Some(Place::Var(_))) {
            // May need let for first assignment of ret-only+inout path handled above.
            if declares {
                self.line(depth, &format!("let {lhs} = {call};"));
            } else {
                self.line(depth, &format!("{lhs} = {call};"));
            }
        } else if targets.len() == 1 {
            self.line(depth, &format!("{lhs} = {call};"));
        } else {
            // Multi-value: declare ret var if needed, then destructure.
            if ret_declares {
                if let Some(Place::Var(v)) = target {
                    self.line(depth, &format!("let {v};"));
                }
            } else if indirect.is_some() {
                // The ret value flows through the synthetic `_sudo_r` temp
                // before being written into a non-Var place (Index/Field);
                // it's always fresh here, so it must always be declared.
                self.line(depth, "let _sudo_r;");
            }
            self.line(depth, &format!("{lhs} = {call};"));
        }
        if let Some(p) = indirect {
            self.assign_to_place(p, "_sudo_r", depth, false);
        }
    }

    fn assign_to_place(&mut self, place: &Place, value: &str, depth: usize, declares: bool) {
        match place {
            Place::Var(n) => {
                if declares && self.ret_slot.is_none() {
                    self.line(depth, &format!("let {n} = {value};"));
                } else {
                    self.line(depth, &format!("{n} = {value};"));
                }
            }
            Place::Index {
                base,
                base_ty,
                index,
            } => {
                let b = self.place_mut_expr(base, depth);
                let i = self.expr(index, depth);
                match strip(base_ty) {
                    Ty::Map(..) => self.line(depth, &format!("{b}.set({i}, {value});")),
                    _ => self.line(depth, &format!("_rt.put({b}, {i}, {value});")),
                }
            }
            Place::Field { base, name, .. } => {
                let b = self.place_mut_expr(base, depth);
                self.line(depth, &format!("{b}.{name} = {value};"));
            }
        }
    }

    /// Inout argument that is a field path: walk GetField through `field_mut`.
    fn extract_mut(&mut self, e: &IrExpr, depth: usize) -> String {
        match &e.kind {
            IrExprKind::Local(n) => n.clone(),
            IrExprKind::GetField { recv, name } => {
                let b = self.extract_mut(recv, depth);
                format!("_rt.field_mut({b}, \"{name}\")")
            }
            _ => self.expr(e, depth),
        }
    }

    /// A place used as a write. Index steps go through `at_mut` / `map_at_mut`.
    fn place_mut_expr(&mut self, place: &Place, depth: usize) -> String {
        match place {
            Place::Var(n) => n.clone(),
            Place::Index {
                base,
                base_ty,
                index,
            } => {
                let b = self.place_mut_expr(base, depth);
                let i = self.expr(index, depth);
                match strip(base_ty) {
                    Ty::Map(..) => format!("_rt.map_at_mut({b}, {i})"),
                    _ => format!("_rt.at_mut({b}, {i})"),
                }
            }
            Place::Field { base, name, .. } => {
                let b = self.place_mut_expr(base, depth);
                format!("_rt.field_mut({b}, \"{name}\")")
            }
        }
    }

    fn variant_field_ty(&self, enum_name: &str, variant: &str, i: usize) -> Option<Ty> {
        // Option/Result field types are not in the module enum table; payloads
        // are copied at extraction when needs_dup on the binder type — for
        // built-ins we don't know the payload ty from the enum def, so rely on
        // IrExpr types only at other copy points. Match binders for Option
        // payload: we look up nothing here for Option/Result.
        if enum_name == "Option" || enum_name == "Result" {
            return None;
        }
        let e = self.enum_def(enum_name);
        let v = e
            .variants
            .iter()
            .find(|v| v.name == variant)
            .unwrap_or_else(|| {
                panic!("internal error: variant '{variant}' not on enum '{enum_name}'")
            });
        v.fields.get(i).map(|f| f.ty.clone())
    }

    fn store(&mut self, e: &IrExpr, depth: usize) -> String {
        if self.lower && arith_root(e) {
            return self.checked_int(e, depth);
        }
        let at = std::ptr::from_ref(e);
        if self.own.takes.contains(&at) {
            if let IrExprKind::Index { recv, index } = &e.kind {
                let r = self.expr(recv, depth);
                let i = self.expr(index, depth);
                return match strip(&recv.ty) {
                    Ty::Map(..) => format!("_rt.map_at_mut({r}, {i})"),
                    _ => format!("_rt.at_mut({r}, {i})"),
                };
            }
        }
        let code = self.expr(e, depth);
        let steal_last = self.own.last_reads.contains(&at)
            && matches!(&e.kind, IrExprKind::Local(n) if self.param_steals(&self.cur_fn, n));
        if aliasing(&e.kind)
            && self.needs_dup(&e.ty)
            && !self.own.moves.contains(&at)
            && !steal_last
        {
            format!("_rt.dup({code})")
        } else {
            code
        }
    }

    fn expr(&mut self, e: &IrExpr, depth: usize) -> String {
        self.expr_prec(e, 0, depth)
    }

    /// Emit with parentheses if this expression binds looser than `ctx`.
    ///
    /// JS precedence (low → high binding strength), NOT Python's:
    ///   1 = `||`, 2 = `&&`, 3 = `===`/`!==`, 4 = `<`/`<=`/`>`/`>=`,
    ///   5 = `+`/`-`, 6 = `*`/`/`/`%`, 7 = unary `!`/`-`, 9 = atom.
    /// Critical differences from Python: `!` binds tighter than every binary
    /// op (so `not (a == b)` needs `!(a === b)`), and equality is looser than
    /// relational (so `a < b === c < d` is `(a < b) === (c < d)` without
    /// forcing shared-tier parens the way Python comparisons do).
    fn expr_prec(&mut self, e: &IrExpr, ctx: u8, depth: usize) -> String {
        let (code, prec) = self.expr_raw(e, depth);
        if prec < ctx {
            format!("({code})")
        } else {
            code
        }
    }

    fn expr_raw(&mut self, e: &IrExpr, depth: usize) -> (String, u8) {
        let atom = 9u8;
        match &e.kind {
            IrExprKind::Int(v) => {
                if self.lower && !self.force_i64 {
                    if safe_i64(*v) {
                        let s = render_i64(*v);
                        (s, if *v < 0 { 7 } else { atom })
                    } else {
                        // Not exact as a number. Arithmetic stores fall back
                        // to BigInt; a mixed bigint literal here would throw.
                        ("NaN".into(), atom)
                    }
                } else {
                    (format!("{v}n"), if *v < 0 { 7 } else { atom })
                }
            }
            IrExprKind::Float(v) => {
                let s = js_float_lit(*v);
                (s, if *v < 0.0 { 7 } else { atom })
            }
            IrExprKind::Bool(v) => (if *v { "true" } else { "false" }.into(), atom),
            IrExprKind::Text(s) => (format!("_rt.txt({})", js_text_str(s)), atom),
            IrExprKind::Local(n) | IrExprKind::Const(n) | IrExprKind::FuncRef(n) => {
                if self.force_i64
                    && matches!(e.ty, Ty::Int)
                    && matches!(e.kind, IrExprKind::Local(_))
                {
                    (format!("_rt.as_i64({n})"), atom)
                } else {
                    (n.clone(), atom)
                }
            }
            IrExprKind::List(xs) => {
                let items: Vec<String> = xs.iter().map(|x| self.store(x, depth)).collect();
                (format!("_rt.lst([{}])", items.join(", ")), atom)
            }
            IrExprKind::Tuple(xs) => {
                // Tuples stay JS arrays. Lists are CowList — do not wrap tuples.
                let items: Vec<String> = xs.iter().map(|x| self.store(x, depth)).collect();
                (format!("[{}]", items.join(", ")), atom)
            }
            IrExprKind::CallFunc { name, args } => {
                if self.want_inline(name) {
                    if let Some(func) = self.resolve_func(name).cloned() {
                        let tmp = self.fresh("r");
                        self.line(depth, &format!("let {tmp};"));
                        self.emit_inline(&func, args, RetSlot::Name(tmp.clone()), depth);
                        return (tmp, atom);
                    }
                }
                (self.emit_call(name, args, false, depth), atom)
            }
            IrExprKind::CallValue { callee, args } => {
                let c = self.expr_prec(callee, atom, depth);
                let a: Vec<String> = args.iter().map(|x| self.expr(x, depth)).collect();
                (format!("{c}({})", a.join(", ")), atom)
            }
            IrExprKind::NewRecord { name, args } => {
                let a: Vec<String> = args.iter().map(|x| self.store(x, depth)).collect();
                let new = format!("new {}({})", self.type_qual(name), a.join(", "));
                if self.shared.contains(name) {
                    (new, atom)
                } else {
                    (format!("_rt.rec({new})"), atom)
                }
            }
            IrExprKind::NewVariant {
                enum_name,
                variant,
                args,
            } => {
                let a: Vec<String> = args.iter().map(|x| self.store(x, depth)).collect();
                let code = match (enum_name.as_str(), variant.as_str()) {
                    ("Option", "Some") => format!("new _rt.Some({})", a.join(", ")),
                    ("Option", "None") => "_rt.NONE".to_string(),
                    ("Result", "Ok") => format!("new _rt.Ok({})", a.join(", ")),
                    ("Result", "Err") => format!("new _rt.Err({})", a.join(", ")),
                    _ => format!(
                        "new {}({})",
                        self.variant_qual(enum_name, variant),
                        a.join(", ")
                    ),
                };
                (code, atom)
            }
            IrExprKind::Builtin { builtin, args } => (self.builtin(*builtin, args, depth), atom),
            IrExprKind::MutBuiltin {
                builtin,
                recv,
                args,
                ..
            } => (self.mut_builtin(*builtin, recv, args, depth), atom),
            IrExprKind::GetField { recv, name } => {
                let r = self.expr_prec(recv, atom, depth);
                let raw = format!("{r}.{name}");
                if self.lower && matches!(e.ty, Ty::Int) && !self.is_mut_field(e) {
                    // A record that is not updated in place still stores BigInt.
                    (Self::num_coerce(&raw), atom)
                } else if self.needs_bigint_read(e) {
                    (format!("_rt.as_i64({raw})"), atom)
                } else {
                    (raw, atom)
                }
            }
            IrExprKind::Index { recv, index } => {
                if let Some(code) = self.fast_index(recv, index, &e.ty) {
                    return (code, atom);
                }
                let r = self.expr_prec(recv, atom, depth);
                let i = if matches!(index.ty, Ty::Int) {
                    self.bigint_one(index, depth)
                } else {
                    self.expr(index, depth)
                };
                match strip(&recv.ty) {
                    Ty::Map(..) => (format!("{r}.get({i})"), atom),
                    _ => {
                        let at = format!("_rt.at({r}, {i})");
                        if self.needs_bigint_read(e) {
                            (format!("_rt.as_i64({at})"), atom)
                        } else {
                            (at, atom)
                        }
                    }
                }
            }
            IrExprKind::Unary { op, operand } => match op {
                UnaryOp::Neg => match strip(&e.ty) {
                    Ty::Int if self.lower => {
                        if let Some(v) = fold_i64(e) {
                            let s = if safe_i64(v) {
                                render_i64(v)
                            } else {
                                "NaN".into()
                            };
                            return (s, if v < 0 { 7 } else { atom });
                        }
                        let x = self.expr_prec(operand, 5, depth);
                        (format!("(0 - {x})"), 5)
                    }
                    Ty::Int => {
                        let x = self.expr(operand, depth);
                        (format!("_rt.neg({x})"), atom)
                    }
                    _ => {
                        // Unary `-` binds tighter than every binary; paren any binary operand.
                        let x = self.expr_prec(operand, 7, depth);
                        // `-(-a)` / `-(-0.0)` must not collapse into the JS
                        // pre-decrement `--` (silently wrong, or a SyntaxError on
                        // a literal). Parenthesize when the operand renders with a
                        // leading `-`. (F4)
                        if x.starts_with('-') {
                            (format!("-({x})"), 7)
                        } else {
                            (format!("-{x}"), 7)
                        }
                    }
                },
                UnaryOp::Not => {
                    // JS `!` is unary-tier (unlike Python `not`, which is looser
                    // than comparisons). Force parens around any binary operand:
                    // `not (a == b)` → `!(a === b)`, never `!a === b`.
                    let x = self.expr_prec(operand, 7, depth);
                    (format!("!{x}"), 7)
                }
            },
            IrExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, &e.ty, depth),
        }
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: &IrExpr,
        rhs: &IrExpr,
        result_ty: &Ty,
        depth: usize,
    ) -> (String, u8) {
        let is_int = matches!(strip(&lhs.ty), Ty::Int);
        let is_float = matches!(strip(&lhs.ty), Ty::Float);
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let sym = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    _ => "*",
                };
                let prec = if sym == "*" { 6 } else { 5 };
                if is_int && self.lower {
                    let l = self.expr_prec(lhs, prec, depth);
                    let r = self.expr_prec(rhs, prec + 1, depth);
                    (format!("{l} {sym} {r}"), prec)
                } else if is_int {
                    let l = self.expr_prec(lhs, prec, depth);
                    let r = self.expr_prec(rhs, prec + 1, depth);
                    (format!("_rt.chk({l} {sym} {r})"), 9)
                } else if is_float {
                    let l = self.expr_prec(lhs, prec, depth);
                    let r = self.expr_prec(rhs, prec + 1, depth);
                    (format!("{l} {sym} {r}"), prec)
                } else {
                    // List concatenation.
                    let l = self.expr_prec(lhs, 9, depth);
                    let r = self.expr_prec(rhs, 9, depth);
                    if self.needs_dup(result_ty) {
                        (format!("_rt.dup({l}.concat({r}))"), 9)
                    } else {
                        (format!("{l}.concat({r})"), 9)
                    }
                }
            }
            BinaryOp::Div => {
                if is_int {
                    let (l, r) = self.bigint_pair(lhs, rhs, depth);
                    (format!("_rt.div({l}, {r})"), 9)
                } else {
                    let l = self.expr(lhs, depth);
                    let r = self.expr(rhs, depth);
                    (format!("_rt.fdiv({l}, {r})"), 9)
                }
            }
            BinaryOp::Mod => {
                let (l, r) = self.bigint_pair(lhs, rhs, depth);
                (format!("_rt.mod_i64({l}, {r})"), 9)
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let sym = match op {
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    _ => ">=",
                };
                if is_int && self.lower {
                    let l = self.expr_prec(lhs, 4, depth);
                    let r = self.expr_prec(rhs, 5, depth);
                    return (format!("{l} {sym} {r}"), 4);
                }
                if is_int && (self.is_mut_field(lhs) || self.is_mut_field(rhs)) {
                    let l = self.mut_cmp_side(lhs, depth);
                    let r = self.mut_cmp_side(rhs, depth);
                    return (format!("({l} {sym} {r})"), 9);
                }
                // Relational tier 4 (tighter than equality tier 3 in JS).
                let l = self.expr_prec(lhs, 4, depth);
                let r = self.expr_prec(rhs, 5, depth);
                (format!("{l} {sym} {r}"), 4)
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                if is_int && self.lower {
                    let sym = if op == BinaryOp::Eq { "===" } else { "!==" };
                    let l = self.expr_prec(lhs, 3, depth);
                    let r = self.expr_prec(rhs, 4, depth);
                    return (format!("{l} {sym} {r}"), 3);
                }
                if is_int && (self.is_mut_field(lhs) || self.is_mut_field(rhs)) {
                    let sym = if op == BinaryOp::Eq { "===" } else { "!==" };
                    let l = self.mut_cmp_side(lhs, depth);
                    let r = self.mut_cmp_side(rhs, depth);
                    return (format!("({l} {sym} {r})"), 9);
                }
                if scalar_eq(&lhs.ty) {
                    let sym = if op == BinaryOp::Eq { "===" } else { "!==" };
                    // Equality tier 3 — looser than relational, so
                    // `a < b === c < d` needs no extra parens on the relations.
                    let l = self.expr_prec(lhs, 3, depth);
                    let r = self.expr_prec(rhs, 4, depth);
                    (format!("{l} {sym} {r}"), 3)
                } else {
                    let l = self.expr(lhs, depth);
                    let r = self.expr(rhs, depth);
                    let call = format!("_rt.eq({l}, {r})");
                    if op == BinaryOp::Eq {
                        (call, 9)
                    } else {
                        // `!` is unary-tier 7 (same as UnaryOp::Not).
                        (format!("!{call}"), 7)
                    }
                }
            }
            BinaryOp::And => {
                let l = self.expr_prec(lhs, 2, depth);
                let r = self.expr_prec(rhs, 3, depth);
                (format!("{l} && {r}"), 2)
            }
            BinaryOp::Or => {
                let l = self.expr_prec(lhs, 1, depth);
                let r = self.expr_prec(rhs, 2, depth);
                (format!("{l} || {r}"), 1)
            }
        }
    }

    fn builtin(&mut self, b: Builtin, args: &[IrExpr], depth: usize) -> String {
        let a = |s: &mut Self, i: usize| s.expr(&args[i], depth);
        let a_atom = |s: &mut Self, i: usize| s.expr_prec(&args[i], 9, depth);
        match b {
            Builtin::AbsInt => format!("_rt.abs_i64({})", self.bigint_one(&args[0], depth)),
            // Host builtins via globalThis: a sudo record named BigInt/Number/Math
            // (e.g. stdlib/bigint.sudo) would otherwise shadow the global and
            // turn `BigInt(xs.length)` into a class call without `new`.
            Builtin::AbsFloat => format!("globalThis.Math.abs({})", a(self, 0)),
            Builtin::MinInt => {
                let (x, y) = self.bigint_pair(&args[0], &args[1], depth);
                format!("(({x}) < ({y}) ? ({x}) : ({y}))")
            }
            Builtin::MaxInt => {
                let (x, y) = self.bigint_pair(&args[0], &args[1], depth);
                format!("(({x}) > ({y}) ? ({x}) : ({y}))")
            }
            Builtin::MinFloat => format!("_rt.fmin({}, {})", a(self, 0), a(self, 1)),
            Builtin::MaxFloat => format!("_rt.fmax({}, {})", a(self, 0), a(self, 1)),
            Builtin::FloatOfInt => format!("globalThis.Number({})", a(self, 0)),
            Builtin::IntOfFloat => format!("_rt.int_of({})", a(self, 0)),
            Builtin::Floor => format!("_rt.floor({})", a(self, 0)),
            Builtin::Ceil => format!("_rt.ceil({})", a(self, 0)),
            Builtin::Round => format!("_rt.round_half_away({})", a(self, 0)),
            Builtin::Sqrt => format!("_rt.sqrt({})", a(self, 0)),
            Builtin::Filled => {
                let n = a(self, 0);
                let v = self.expr(&args[1], depth);
                format!("_rt.filled({n}, {v})")
            }
            Builtin::NewMap => "new _rt.SudoMap()".into(),
            Builtin::NewSet => "new _rt.SudoSet()".into(),
            Builtin::ListLength => {
                format!("globalThis.BigInt({}.length)", a_atom(self, 0))
            }
            Builtin::MapSize | Builtin::SetSize => {
                format!("globalThis.BigInt({}.size)", a_atom(self, 0))
            }
            Builtin::MapGet => format!("{}.get_opt({})", a_atom(self, 0), a(self, 1)),
            Builtin::MapHas => format!("{}.has({})", a_atom(self, 0), a(self, 1)),
            Builtin::MapKeys => format!("{}.keys_list()", a_atom(self, 0)),
            Builtin::MapValues => format!("{}.values_list()", a_atom(self, 0)),
            Builtin::SetHas => format!("{}.has({})", a_atom(self, 0), a(self, 1)),
            Builtin::SetItems => format!("{}.items_list()", a_atom(self, 0)),
            Builtin::OptIsSome => format!("_rt.is_some({})", a(self, 0)),
            Builtin::OptIsNone => format!("_rt.is_none({})", a(self, 0)),
            Builtin::OptUnwrap | Builtin::ResUnwrap => format!("_rt.unwrap({})", a(self, 0)),
            Builtin::OptGetOr | Builtin::ResGetOr => {
                format!("_rt.get_or({}, {})", a(self, 0), a(self, 1))
            }
            Builtin::ResIsOk => format!("_rt.is_ok({})", a(self, 0)),
            Builtin::ResIsErr => format!("_rt.is_err({})", a(self, 0)),
            _ => unreachable!("mutating builtin in non-mut position: {b:?}"),
        }
    }

    fn mut_builtin(&mut self, b: Builtin, recv: &Place, args: &[IrExpr], depth: usize) -> String {
        let r = self.place_mut_expr(recv, depth);
        match b {
            Builtin::ListAppend => {
                if let Some(acc) = self.fast.as_ref().map(|f| f.acc.clone()) {
                    if matches!(recv, Place::Var(n) if n == &acc) {
                        if let Some(code) = self.inplace_append(&args[0], depth) {
                            return code;
                        }
                        let v = self.store(&args[0], depth);
                        return format!("_sudo_d[_sudo_i] = {v}");
                    }
                }
                let v = self.store(&args[0], depth);
                format!("{r}.push({v})")
            }
            Builtin::ListPop => format!("_rt.pop({r})"),
            Builtin::ListInsert => {
                let i = self.expr(&args[0], depth);
                let v = self.store(&args[1], depth);
                format!("_rt.insert({r}, {i}, {v})")
            }
            Builtin::ListRemoveAt => {
                let i = self.expr(&args[0], depth);
                format!("_rt.remove_at({r}, {i})")
            }
            Builtin::ListSwap => {
                let i = self.expr(&args[0], depth);
                let j = self.expr(&args[1], depth);
                format!("_rt.swap({r}, {i}, {j})")
            }
            Builtin::ListSort => format!("_rt.sort({r})"),
            Builtin::MapDelete => {
                let k = self.expr(&args[0], depth);
                format!("{r}.delete({k})")
            }
            Builtin::SetAdd => {
                let v = self.expr(&args[0], depth);
                format!("{r}.add({v})")
            }
            Builtin::SetRemove => {
                let v = self.expr(&args[0], depth);
                format!("{r}.remove({v})")
            }
            _ => unreachable!("non-mutating builtin in mut position: {b:?}"),
        }
    }

    /// Record type this function updates through a parameter, instead of
    /// allocating a new one. Callers that do not own the value copy first.
    fn mut_receiver(&self, name: &str) -> Option<String> {
        let func = self.resolve_func(name)?;
        let Ty::Record(ret) = func.ret.as_ref()? else {
            return None;
        };
        if !self.plan.mut_recs.contains(ret) {
            return None;
        }
        let takes = func
            .params
            .iter()
            .any(|p| matches!(&p.ty, Ty::Record(n) if n == ret));
        if takes { Some(ret.clone()) } else { None }
    }

    fn mut_param_for_return(&self, e: &IrExpr) -> Option<String> {
        let IrExprKind::NewRecord { name, .. } = &e.kind else {
            return None;
        };
        if !self.plan.mut_recs.contains(name) {
            return None;
        }
        let func = self.m.func(&self.cur_fn)?;
        func.params
            .iter()
            .find(|p| matches!(&p.ty, Ty::Record(n) if n == name))
            .map(|p| p.name.clone())
    }

    fn copy_record(&mut self, ty: &str, code: &str, depth: usize) -> Option<String> {
        let fields: Vec<String> = sudoc_ir::find_record(self.all, ty)?
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let t = self.fresh("c");
        let qual = self.type_qual(ty);
        let args: Vec<String> = fields.iter().map(|f| format!("{code}.{f}")).collect();
        self.line(
            depth,
            &format!("const {t} = new {qual}({});", args.join(", ")),
        );
        Some(t)
    }

    /// Inline this call. A list rebuild stays a call so the loop stays small;
    /// the lowered copy lives on the helper.
    fn want_inline(&self, name: &str) -> bool {
        if self.defining_inlineable || self.inline_stack.iter().any(|n| n == name) {
            return false;
        }
        if !self.plan.inlineable.contains(name) || self.mut_receiver(name).is_some() {
            return false;
        }
        // The slow arm of a rebuild stays a call. The rc == 1 arm inlines,
        // which is the shape the optimizing compiler can see through.
        if self.ret_slot.is_none()
            && self.fast.is_none()
            && self.plan.rebuilds.contains_key(&self.cur_fn)
        {
            return false;
        }
        true
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("_sudo_{prefix}{}", self.seq)
    }

    fn param_steals(&self, func: &str, param: &str) -> bool {
        self.plan
            .steals
            .get(func)
            .is_some_and(|s| s.contains(param))
    }

    fn callee_steals(&self, func: &str, arg: &IrExpr) -> bool {
        let Some(f) = self.resolve_func(func) else {
            return false;
        };
        let Some(p) = f.params.first() else {
            return false;
        };
        matches!(&arg.kind, IrExprKind::Local(_)) && self.param_steals(func, &p.name)
    }

    /// `transfer` is true when the first argument's storage is the value
    /// being overwritten by this call (`x = f(x)` / `return f(x)`).
    fn emit_call(&mut self, name: &str, args: &[IrExpr], transfer: bool, depth: usize) -> String {
        let params: Vec<(String, Ty)> = self
            .resolve_func(name)
            .map(|f| {
                f.params
                    .iter()
                    .map(|p| (p.name.clone(), p.ty.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let mut code = self.expr(a, depth);
            let steals = params.get(i).is_some_and(|(pname, ty)| {
                matches!(ty, Ty::List(_)) && self.param_steals(name, pname)
            });
            if steals && !(transfer && i == 0) {
                let t = self.fresh("s");
                self.line(depth, &format!("const {t} = {code};"));
                code = format!("_rt.dup({t})");
            }
            // A shared list's elements are the same objects. Copy before a
            // function that writes the record it is given. The rc == 1 loop
            // is the only caller allowed to update the element directly.
            if i == 0 && self.fast.is_none() {
                if let Some(ty) = self.mut_receiver(name) {
                    if let Some(copied) = self.copy_record(&ty, &code, depth) {
                        code = copied;
                    }
                }
            }
            let callee_lowered = self.plan.lowered.contains(name);
            if self.lower && matches!(a.ty, Ty::Int) && !callee_lowered {
                let t = self.fresh("i");
                self.line(depth, &format!("const {t} = {code};"));
                code = format!("(typeof {t} === \"bigint\" ? {t} : globalThis.BigInt({t}))");
            }
            parts.push(code);
        }
        self.wrap_lowered_return(name, &format!("{name}({})", parts.join(", ")), depth)
    }

    /// A lowered helper may return a safe integer as a number. Callers that
    /// still speak BigInt convert at the call.
    fn wrap_lowered_return(&mut self, name: &str, call: &str, depth: usize) -> String {
        if self.lower || !self.plan.lowered.contains(name) {
            return call.to_string();
        }
        let Some(func) = self.resolve_func(name) else {
            return call.to_string();
        };
        let ret = func.ret.clone();
        match &ret {
            Some(Ty::Int) => {
                let t = self.fresh("o");
                self.line(depth, &format!("const {t} = {call};"));
                format!("(typeof {t} === \"bigint\" ? {t} : globalThis.BigInt({t}))")
            }
            Some(Ty::Tuple(ts)) if ts.iter().any(|t| matches!(t, Ty::Int)) => {
                let t = self.fresh("o");
                self.line(depth, &format!("const {t} = {call};"));
                let bits: Vec<String> = ts
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| {
                        if matches!(ty, Ty::Int) {
                            format!(
                                "(typeof {t}[{i}] === \"bigint\" ? {t}[{i}] : globalThis.BigInt({t}[{i}]))"
                            )
                        } else {
                            format!("{t}[{i}]")
                        }
                    })
                    .collect();
                format!("[{}]", bits.join(", "))
            }
            _ => call.to_string(),
        }
    }

    fn emit_inline(&mut self, func: &IrFunc, args: &[IrExpr], dest: RetSlot, depth: usize) {
        self.inline_stack.push(func.name.clone());
        self.seq += 1;
        let id = self.seq;
        // Rename first, then substitute. Walking the caller's expressions
        // through the helper's rename map would rewrite the caller's locals.
        let mut map = std::collections::HashMap::new();
        for p in &func.params {
            map.insert(p.name.clone(), format!("_sudo_p{id}_{}", p.name));
        }
        let locals = inline::declared_locals(&func.body);
        for local in &locals {
            map.entry(local.clone())
                .or_insert_with(|| format!("_sudo_l{id}_{local}"));
        }
        // Source order, not HashMap order: two emits of one module must match.
        let mut seen_let = std::collections::HashSet::new();
        let hoist_lets = self.capture_loop;
        if hoist_lets {
            self.divert_hoist = true;
        }
        for local in &locals {
            if let Some(name) = map.get(local) {
                if name.starts_with(&format!("_sudo_l{id}_")) && seen_let.insert(name.clone()) {
                    self.line(depth, &format!("let {name};"));
                }
            }
        }
        if hoist_lets {
            self.divert_hoist = false;
        }
        let body = inline::rename_body(&func.body, &map);
        let mut subst = std::collections::HashMap::new();
        for (p, a) in func.params.iter().zip(args.iter()) {
            let fresh = map.get(&p.name).unwrap().clone();
            let callee_lowered = self.plan.lowered.contains(&func.name);
            let number_leaf = self.is_mut_field(a)
                || fold_i64(a).is_some()
                || (callee_lowered && self.lower && matches!(a.kind, IrExprKind::Local(_)));
            if p.never_written && inline::subst_pure(a) && (number_leaf || !matches!(a.ty, Ty::Int))
            {
                subst.insert(fresh, a.clone());
            } else {
                let code = if callee_lowered && matches!(a.ty, Ty::Int) {
                    self.coerce_int_expr(a, depth)
                } else {
                    self.expr(a, depth)
                };
                self.line(depth, &format!("let {fresh} = {code};"));
            }
        }
        let body = inline::subst_locals(&body, &subst);
        let saved_def = self.defining_inlineable;
        self.defining_inlineable = false;
        let structured = returns_structured(&body);
        let saved_done = self.done_flag.clone();
        if structured {
            self.done_flag = None;
        } else {
            let done = format!("_sudo_done{id}");
            self.line(depth, &format!("let {done} = false;"));
            self.done_flag = Some(done);
        }
        let saved_lower = self.lower;
        let saved_ret = self.ret_slot.take();
        let saved_own = std::mem::take(&mut self.own);
        let saved_fn = std::mem::take(&mut self.cur_fn);
        // Only a helper selected for number stores is lowered. Inlining a
        // function that divides or mods must keep the BigInt compares.
        self.lower = self.plan.lowered.contains(&func.name);
        self.cur_fn = func.name.clone();
        self.ret_slot = Some(dest.clone());
        self.block(&body, depth, &[]);
        self.lower = saved_lower;
        self.ret_slot = saved_ret;
        self.done_flag = saved_done;
        self.own = saved_own;
        self.cur_fn = saved_fn;
        self.inline_stack.pop();
        self.defining_inlineable = saved_def;
        if !saved_lower {
            self.boundary_int(func, &dest, depth);
        }
    }

    fn boundary_int(&mut self, func: &IrFunc, dest: &RetSlot, depth: usize) {
        let convert =
            |n: &str| format!("{n} = typeof {n} === \"bigint\" ? {n} : globalThis.BigInt({n});");
        match dest {
            RetSlot::Name(n) if matches!(func.ret, Some(Ty::Int)) => {
                self.line(depth, &convert(n));
            }
            RetSlot::Tuple(names) => {
                if let Some(Ty::Tuple(ts)) = &func.ret {
                    for (n, t) in names.iter().zip(ts.iter()) {
                        if matches!(t, Ty::Int) {
                            self.line(depth, &convert(n));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn emit_inline_return(&mut self, v: Option<&IrExpr>, depth: usize) {
        self.emit_return_into_slot(v, depth);
        if let Some(flag) = &self.done_flag {
            self.line(depth, &format!("{flag} = true;"));
        }
    }

    fn emit_return_into_slot(&mut self, v: Option<&IrExpr>, depth: usize) {
        let Some(slot) = self.ret_slot.clone() else {
            return;
        };
        let lowered = |this: &mut Self, e: &IrExpr| -> String {
            // store, not expr: an inlined `return bench` must still dup.
            // An arithmetic int store checks once, then falls back to BigInt.
            this.store(e, depth)
        };
        match (&slot, v) {
            (RetSlot::Name(n), Some(e)) => {
                if matches!(&e.kind, IrExprKind::Local(src) if src == n) {
                    return;
                }
                let code = lowered(self, e);
                self.line(depth, &format!("{n} = {code};"));
            }
            (RetSlot::Tuple(names), Some(e)) => {
                if let IrExprKind::Tuple(xs) = &e.kind {
                    for (name, x) in names.iter().zip(xs.iter()) {
                        if matches!(&x.kind, IrExprKind::Local(src) if src == name) {
                            continue;
                        }
                        let code = lowered(self, x);
                        self.line(depth, &format!("{name} = {code};"));
                    }
                } else if let IrExprKind::CallFunc { name, args } = &e.kind {
                    if self.want_inline(name) {
                        if let Some(func) = self.resolve_func(name).cloned() {
                            self.emit_inline(&func, args, RetSlot::Tuple(names.clone()), depth);
                            return;
                        }
                    }
                    let code = self.expr(e, depth);
                    self.line(depth, &format!("[{}] = {code};", names.join(", ")));
                } else {
                    let code = self.expr(e, depth);
                    self.line(depth, &format!("[{}] = {code};", names.join(", ")));
                }
            }
            (RetSlot::InPlace(elem), Some(e)) => {
                if let IrExprKind::NewRecord { name, args } = &e.kind {
                    if self.plan.mut_recs.contains(name) {
                        self.write_rec_fields(name, args, elem, depth);
                        return;
                    }
                }
                let code = self.store(e, depth);
                self.line(depth, &format!("{elem} = {code};"));
            }
            _ => {}
        }
    }

    fn emit_fast_rebuild(&mut self, reb: &inline::Rebuild, depth: usize) {
        let src = &reb.src;
        self.line(
            depth,
            &format!("if ({src} instanceof _rt.CowList && {src}._box.rc === 1) {{"),
        );
        self.line(depth + 1, &format!("const _sudo_d = {src}._box.d;"));
        self.fast = Some(FastPath {
            src: reb.src.clone(),
            index: reb.index.clone(),
            acc: reb.acc.clone(),
            elem: "_sudo_e".into(),
        });
        // The loop body is straight-line once its helpers are inlined, so
        // integer stores there use the number path. `rc != 1` keeps BigInt.
        // Reads of the matrix (and anything else that does not touch the
        // element) are emitted once, ahead of the loop.
        let saved_lower = self.lower;
        self.lower = true;
        self.capture_loop = true;
        self.divert_hoist = false;
        self.hoist_buf.clear();
        self.loop_buf.clear();
        self.invariant.clear();
        let kept: Vec<String> = {
            let src_name = reb.src.clone();
            let index_name = reb.index.clone();
            self.m
                .func(&self.cur_fn)
                .map(|f| {
                    f.params
                        .iter()
                        .filter(|p| p.name != src_name && p.name != index_name)
                        .map(|p| p.name.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        self.invariant.extend(kept);
        self.block(&reb.body, depth + 2, &[]);
        self.capture_loop = false;
        self.divert_hoist = false;
        self.lower = saved_lower;
        self.fast = None;
        self.out.push_str(&self.hoist_buf);
        self.line(
            depth + 1,
            "for (let _sudo_i = 0; _sudo_i < _sudo_d.length; _sudo_i++) {",
        );
        if uses_index_value(&reb.body, &reb.index) {
            self.line(
                depth + 2,
                &format!("let {} = globalThis.BigInt(_sudo_i);", reb.index),
            );
        }
        self.line(depth + 2, "const _sudo_e = _sudo_d[_sudo_i];");
        self.out.push_str(&self.loop_buf);
        self.line(depth + 1, "}");
        self.line(depth + 1, &format!("return {src};"));
        self.line(depth, "}");
    }

    fn fast_index(&self, recv: &IrExpr, index: &IrExpr, ty: &Ty) -> Option<String> {
        let IrExprKind::Local(r) = &recv.kind else {
            return None;
        };
        let IrExprKind::Local(i) = &index.kind else {
            return None;
        };
        let raw = if let Some(fast) = &self.fast {
            if r == &fast.src && i == &fast.index {
                "_sudo_d[_sudo_i]".to_string()
            } else {
                return self.direct_index(r, i, ty);
            }
        } else {
            return self.direct_index(r, i, ty);
        };
        if !self.lower && matches!(ty, Ty::Int) && !self.is_mut_elem(ty) {
            Some(format!("_rt.as_i64({raw})"))
        } else {
            Some(raw)
        }
    }

    fn direct_index(&self, recv: &str, index: &str, ty: &Ty) -> Option<String> {
        let d = self.direct.as_ref()?;
        if recv != d.src || index != d.index {
            return None;
        }
        let raw = format!("{}[{}]", d.arr, d.ix);
        if !self.lower && matches!(ty, Ty::Int) {
            Some(format!("_rt.as_i64({raw})"))
        } else {
            Some(raw)
        }
    }

    fn is_mut_elem(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Record(n) if self.plan.mut_recs.contains(n))
    }

    /// A read that must be a BigInt: outside a lowered region, or inside a
    /// bigint-only operation (`div`, `mod`, an index). Fields of a record
    /// updated in place are already numbers.
    fn needs_bigint_read(&self, e: &IrExpr) -> bool {
        // Lowered code reads number fields raw. Everywhere else an int is a
        // BigInt, including a number stored on a record the fast path writes.
        matches!(e.ty, Ty::Int) && (self.force_i64 || !self.lower)
    }

    fn bigint_one(&mut self, e: &IrExpr, depth: usize) -> String {
        if !self.lower {
            return self.expr(e, depth);
        }
        let saved_l = self.lower;
        let saved_f = self.force_i64;
        self.lower = false;
        self.force_i64 = true;
        let code = self.expr(e, depth);
        self.lower = saved_l;
        self.force_i64 = saved_f;
        code
    }

    fn bigint_pair(&mut self, lhs: &IrExpr, rhs: &IrExpr, depth: usize) -> (String, String) {
        let l = self.bigint_one(lhs, depth);
        let r = self.bigint_one(rhs, depth);
        (l, r)
    }

    fn is_mut_field(&self, e: &IrExpr) -> bool {
        let IrExprKind::GetField { recv, .. } = &e.kind else {
            return false;
        };
        matches!(&recv.ty, Ty::Record(n) if self.plan.mut_recs.contains(n))
    }

    fn field_text(&self, e: &IrExpr) -> Option<String> {
        let IrExprKind::GetField { recv, name } = &e.kind else {
            return None;
        };
        let IrExprKind::Local(n) = &recv.kind else {
            return None;
        };
        Some(format!("{n}.{name}"))
    }

    fn coerce_param(&mut self, name: &str, ty: &Ty, depth: usize) {
        match ty {
            Ty::Int => self.line(
                depth,
                &format!(
                    "if (typeof {name} === \"bigint\" && {name} >= {SAFE_MIN} && {name} <= {SAFE_MAX}) {name} = globalThis.Number({name});"
                ),
            ),
            Ty::Tuple(ts) if ts.iter().any(tuple_has_int) => {
                self.line(depth, &format!("{name} = _rt.nums({name});"));
            }
            _ => {}
        }
    }

    fn coerce_int_expr(&mut self, e: &IrExpr, depth: usize) -> String {
        if let Some(v) = fold_i64(e) {
            return if safe_i64(v) {
                render_i64(v)
            } else {
                format!("{v}n")
            };
        }
        if self.is_mut_field(e) {
            if let Some(s) = self.field_text(e) {
                return s;
            }
        }
        let code = self.expr(e, depth);
        format!(
            "(typeof ({code}) === \"number\" ? ({code}) : (typeof ({code}) === \"bigint\" && ({code}) >= {SAFE_MIN} && ({code}) <= {SAFE_MAX} ? globalThis.Number({code}) : ({code})))"
        )
    }

    /// Compare against a number field from outside a lowered function.
    fn mut_cmp_side(&mut self, e: &IrExpr, depth: usize) -> String {
        if let Some(v) = fold_i64(e) {
            return if safe_i64(v) {
                render_i64(v)
            } else {
                format!("{v}n")
            };
        }
        if self.is_mut_field(e) {
            if let Some(s) = self.field_text(e) {
                return s;
            }
        }
        self.coerce_int_expr(e, depth)
    }

    fn write_rec_fields(&mut self, name: &str, args: &[IrExpr], elem: &str, depth: usize) {
        let Some(rec) = sudoc_ir::find_record(self.all, name) else {
            return;
        };
        for (f, a) in rec.fields.iter().zip(args.iter()) {
            let v = self.store(a, depth);
            self.line(depth, &format!("{elem}.{} = {v};", f.name));
        }
    }

    /// `out.append(Cubie(...))` on the unique-list path writes the fields of
    /// the element already in the slot.
    fn inplace_append(&mut self, value: &IrExpr, depth: usize) -> Option<String> {
        let elem = self.fast.as_ref()?.elem.clone();
        match &value.kind {
            IrExprKind::NewRecord { name, args } if self.plan.mut_recs.contains(name) => {
                self.write_rec_fields(name, args, &elem, depth);
                Some(format!("_sudo_d[_sudo_i] = {elem}"))
            }
            IrExprKind::CallFunc { name, args } if self.want_inline(name) => {
                let func = self.resolve_func(name)?.clone();
                let returns = matches!(&func.ret, Some(Ty::Record(r)) if self.plan.mut_recs.contains(r));
                if !returns {
                    return None;
                }
                self.emit_inline(&func, args, RetSlot::InPlace(elem.clone()), depth);
                Some(format!("_sudo_d[_sudo_i] = {elem}"))
            }
            _ => None,
        }
    }

    fn emit_direct_loop(
        &mut self,
        src: &str,
        var: &str,
        body: &[IrStmt],
        depth: usize,
        inouts: &[String],
    ) {
        self.seq += 1;
        let id = self.seq;
        let arr = format!("_sudo_d{id}");
        let ix = format!("_sudo_i{id}");
        self.line(depth, "{");
        self.line(depth + 1, &format!("const {arr} = {src}._box.d;"));
        self.line(
            depth + 1,
            &format!("for (let {ix} = 0; {ix} < {arr}.length; {ix}++) {{"),
        );
        if uses_index_value(body, var) {
            self.line(
                depth + 2,
                &format!("let {var} = globalThis.BigInt({ix});"),
            );
        }
        let prev = self.direct.clone();
        self.direct = Some(DirectIndex {
            src: src.to_string(),
            index: var.to_string(),
            arr: arr.clone(),
            ix: ix.clone(),
        });
        self.block(body, depth + 2, inouts);
        self.direct = prev;
        self.line(depth + 1, "}");
        self.line(depth, "}");
    }

    /// One `Number.isSafeInteger` check for a whole arithmetic store.
    /// The number arm is raw `+` `-` `*`. The other arm is `_rt.chk`.
    fn checked_int(&mut self, e: &IrExpr, depth: usize) -> String {
        if let Some(v) = fold_i64(e) {
            return if safe_i64(v) {
                render_i64(v)
            } else {
                format!("{v}n")
            };
        }
        let mut temps = std::collections::HashMap::new();
        self.bind_arith_leaf(e, depth, &mut temps);
        let num = self.plain_num(e, &temps);
        // `x * 0`, `x * 1`, `x + 0` reduce to a leaf. No check: the value is
        // already the result, and a later store checks real arithmetic.
        if !num.contains('+') && !num.contains('*') && !num.contains(" - ") {
            return num;
        }
        let n = self.fresh("n");
        let big = self.plain_big(e, &temps);
        self.line(depth, &format!("const {n} = ({num}) + 0;"));
        format!("(Number.isSafeInteger({n}) ? {n} : {big})")
    }

    fn bind_arith_leaf(
        &mut self,
        e: &IrExpr,
        depth: usize,
        temps: &mut std::collections::HashMap<*const IrExpr, String>,
    ) {
        if fold_i64(e).is_some() {
            return;
        }
        match &e.kind {
            IrExprKind::Binary {
                op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul,
                lhs,
                rhs,
            } if matches!(e.ty, Ty::Int) => {
                self.bind_arith_leaf(lhs, depth, temps);
                self.bind_arith_leaf(rhs, depth, temps);
            }
            IrExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } if matches!(e.ty, Ty::Int) => self.bind_arith_leaf(operand, depth, temps),
            IrExprKind::Local(_) => {}
            IrExprKind::GetField { recv, .. } if matches!(recv.kind, IrExprKind::Local(_)) => {}
            _ => {
                let ptr = std::ptr::from_ref(e);
                if temps.contains_key(&ptr) {
                    return;
                }
                let saved_l = self.lower;
                let saved_f = self.force_i64;
                self.lower = true;
                self.force_i64 = false;
                let code = self.expr(e, depth);
                self.lower = saved_l;
                self.force_i64 = saved_f;
                let t = self.fresh("v");
                self.line(depth, &format!("const {t} = {code};"));
                temps.insert(ptr, t);
            }
        }
    }

    fn plain_num(
        &self,
        e: &IrExpr,
        temps: &std::collections::HashMap<*const IrExpr, String>,
    ) -> String {
        if let Some(v) = fold_i64(e) {
            return if safe_i64(v) {
                render_i64(v)
            } else {
                "NaN".into()
            };
        }
        match &e.kind {
            IrExprKind::Binary { op, lhs, rhs } => {
                let l = self.plain_num(lhs, temps);
                let r = self.plain_num(rhs, temps);
                match op {
                    // Finite ints: these identities hold for the BigInt value too,
                    // so folding them does not skip an overflow.
                    BinaryOp::Add if l == "0" => r,
                    BinaryOp::Add if r == "0" => l,
                    BinaryOp::Sub if r == "0" => l,
                    BinaryOp::Mul if l == "0" || r == "0" => "0".into(),
                    BinaryOp::Mul if l == "1" => r,
                    BinaryOp::Mul if r == "1" => l,
                    BinaryOp::Mul if l == "(-1)" => format!("(0 - {r})"),
                    BinaryOp::Mul if r == "(-1)" => format!("(0 - {l})"),
                    BinaryOp::Add => format!("({l} + {r})"),
                    BinaryOp::Sub => format!("({l} - {r})"),
                    BinaryOp::Mul => format!("({l} * {r})"),
                    _ => self.num_leaf(e, temps),
                }
            }
            IrExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => format!("(0 - {})", self.plain_num(operand, temps)),
            _ => self.num_leaf(e, temps),
        }
    }

    fn num_leaf(
        &self,
        e: &IrExpr,
        temps: &std::collections::HashMap<*const IrExpr, String>,
    ) -> String {
        if self.is_mut_field(e) {
            if let Some(s) = self.field_text(e) {
                return s;
            }
        }
        let raw = if let Some(t) = temps.get(&std::ptr::from_ref(e)) {
            t.clone()
        } else if let IrExprKind::Local(n) = &e.kind {
            // Inside the unique-list loop the operands are numbers (record
            // fields and a tuple coerced at entry). A bare local elsewhere
            // may still be a BigInt, and `number + bigint` throws.
            if self.fast.is_some() {
                return n.clone();
            }
            n.clone()
        } else if let Some(s) = self.field_text(e) {
            return Self::num_coerce(&s);
        } else {
            return "NaN".into();
        };
        Self::num_coerce(&raw)
    }

    fn num_coerce(raw: &str) -> String {
        format!(
            "(typeof {raw} === \"number\" ? {raw} : (typeof {raw} === \"bigint\" && {raw} >= {SAFE_MIN} && {raw} <= {SAFE_MAX} ? globalThis.Number({raw}) : NaN))"
        )
    }

    fn plain_big(
        &self,
        e: &IrExpr,
        temps: &std::collections::HashMap<*const IrExpr, String>,
    ) -> String {
        if let Some(v) = fold_i64(e) {
            return format!("{v}n");
        }
        match &e.kind {
            IrExprKind::Binary { op, lhs, rhs } => {
                let l = self.plain_big(lhs, temps);
                let r = self.plain_big(rhs, temps);
                match op {
                    BinaryOp::Add => format!("_rt.chk({l} + {r})"),
                    BinaryOp::Sub => format!("_rt.chk({l} - {r})"),
                    BinaryOp::Mul => format!("_rt.chk({l} * {r})"),
                    _ => format!("_rt.as_i64({})", self.big_leaf(e, temps)),
                }
            }
            IrExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => format!("_rt.neg({})", self.plain_big(operand, temps)),
            _ => format!("_rt.as_i64({})", self.big_leaf(e, temps)),
        }
    }

    fn big_leaf(
        &self,
        e: &IrExpr,
        temps: &std::collections::HashMap<*const IrExpr, String>,
    ) -> String {
        if let Some(t) = temps.get(&std::ptr::from_ref(e)) {
            return t.clone();
        }
        if let IrExprKind::Local(n) = &e.kind {
            return n.clone();
        }
        if let Some(s) = self.field_text(e) {
            return s;
        }
        "0".into()
    }

    fn needs_dup(&self, ty: &Ty) -> bool {
        match ty {
            Ty::List(_) | Ty::Map(..) | Ty::Set(_) => true,
            Ty::Record(n) => !self.shared.contains(n),
            Ty::Tuple(ts) => ts.iter().any(|t| self.needs_dup(t)),
            _ => false,
        }
    }

    fn line(&mut self, depth: usize, s: &str) {
        let buf = if self.capture_loop {
            if self.divert_hoist {
                &mut self.hoist_buf
            } else {
                &mut self.loop_buf
            }
        } else {
            &mut self.out
        };
        for _ in 0..depth {
            buf.push_str("    ");
        }
        buf.push_str(s);
        buf.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }
}

// ---- helpers ----------------------------------------------------------------

/// True when no destructure binding is written later in the function.
/// Then the source may be unpacked without `dup` — sharing is unobservable
/// because nothing mutates through those names. A `Tuple` RHS still
/// `store()`s each element via `expr()` (parallel assign `a, b = c, d`
/// must not alias `c`); this only elides a *single-source* unpack
/// (`a, s = ys[0]` / `a_s, … = a`).
fn unpack_readonly(targets: &[String], written: &std::collections::HashSet<String>) -> bool {
    !targets.is_empty() && targets.iter().all(|t| !written.contains(t))
}

/// `a, b = b, a` (and longer rotations of distinct locals): the values
/// already exist under those names; the statement only rebinds.
fn tuple_is_local_perm(targets: &[String], value: &IrExpr) -> bool {
    let IrExprKind::Tuple(xs) = &value.kind else {
        return false;
    };
    if xs.len() != targets.len() || xs.is_empty() {
        return false;
    }
    let mut srcs: Vec<&str> = Vec::with_capacity(xs.len());
    for x in xs {
        match &x.kind {
            IrExprKind::Local(n) => srcs.push(n.as_str()),
            _ => return false,
        }
    }
    let mut seen = std::collections::HashSet::new();
    if !srcs.iter().all(|s| seen.insert(*s)) {
        return false;
    }
    let mut t: Vec<&str> = targets.iter().map(String::as_str).collect();
    let mut s = srcs;
    t.sort_unstable();
    s.sort_unstable();
    t == s
}

fn aliasing(kind: &IrExprKind) -> bool {
    match kind {
        IrExprKind::Local(_)
        | IrExprKind::Const(_)
        | IrExprKind::GetField { .. }
        | IrExprKind::Index { .. } => true,
        IrExprKind::Builtin { builtin, .. } => matches!(
            builtin,
            Builtin::OptUnwrap | Builtin::OptGetOr | Builtin::ResUnwrap | Builtin::ResGetOr
        ),
        _ => false,
    }
}

fn dest_can_share_tuple(place: &Place) -> bool {
    matches!(place, Place::Index { .. } | Place::Field { .. })
}

fn is_tuple_ty(ty: &Ty) -> bool {
    matches!(strip(ty), Ty::Tuple(_))
}

fn scalar_eq(ty: &Ty) -> bool {
    matches!(strip(ty), Ty::Int | Ty::Bool | Ty::Float)
}

fn strip(ty: &Ty) -> &Ty {
    ty
}

fn ret_parts(parts: &[String]) -> String {
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        format!("[{}]", parts.join(", "))
    }
}

fn definitely_returns(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Return(_) => true,
        IrStmt::If { arms, else_block } => {
            else_block.as_ref().is_some_and(|e| definitely_returns(e))
                && arms.iter().all(|(_, b)| definitely_returns(b))
        }
        IrStmt::Match { arms, .. } => arms.iter().all(|a| definitely_returns(&a.body)),
        _ => false,
    })
}

fn js_float_lit(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v.is_sign_positive() {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    // Preserve negative zero.
    if v == 0.0 && v.is_sign_negative() {
        return "-0.0".into();
    }
    let s = format!("{v:?}");
    // Ensure a decimal form for integral floats so they stay floats in JS.
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        format!("{s}.0")
    } else {
        s
    }
}

/// Build a double-quoted JS string literal from Unicode scalar values.
fn js_text_str(scalars: &[i64]) -> String {
    let mut s = String::from("\"");
    for &c in scalars {
        match char::from_u32(c as u32) {
            Some('"') => s.push_str("\\\""),
            Some('\\') => s.push_str("\\\\"),
            Some('\n') => s.push_str("\\n"),
            Some('\t') => s.push_str("\\t"),
            Some('\r') => s.push_str("\\r"),
            Some(ch) if (' '..='~').contains(&ch) => s.push(ch),
            Some(ch) if (ch as u32) <= 0xFFFF => {
                s.push_str(&format!("\\u{:04x}", ch as u32));
            }
            Some(ch) => s.push_str(&format!("\\u{{{:x}}}", ch as u32)),
            None => s.push_str("\\ufffd"),
        }
    }
    s.push('"');
    s
}

/// The JavaScript backend, via the SDK contract.
pub struct JsBackend;

impl sudoc_sdk::Backend for JsBackend {
    fn name(&self) -> &'static str {
        "js"
    }

    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Result<Vec<sudoc_sdk::GeneratedFile>, String> {
        let modules: Vec<IrModule> = modules.iter().map(reserved::rename_reserved).collect();
        let shared = shared_records(&modules);
        let (entry, deps) = modules.split_last().expect("entry module");
        let mut out = Vec::new();
        for m in deps {
            out.push(sudoc_sdk::GeneratedFile {
                path: impl_file(&m.name),
                contents: emit_with(m, &modules, &shared, false, with_tests),
            });
            if let Some(api) = emit_api(m, &modules) {
                out.push(sudoc_sdk::GeneratedFile {
                    path: api_file(&m.name),
                    contents: api,
                });
            }
        }
        out.push(sudoc_sdk::GeneratedFile {
            path: impl_file(&entry.name),
            contents: emit_with(entry, &modules, &shared, with_tests, with_tests),
        });
        if let Some(api) = emit_api(entry, &modules) {
            out.push(sudoc_sdk::GeneratedFile {
                path: api_file(&entry.name),
                contents: api,
            });
        }
        Ok(out)
    }

    fn runtime_files(&self) -> Vec<sudoc_sdk::GeneratedFile> {
        vec![sudoc_sdk::GeneratedFile {
            path: RUNTIME_FILE.into(),
            contents: RUNTIME.into(),
        }]
    }

    fn test_recipe(&self, entry: &str) -> sudoc_sdk::TestRecipe {
        sudoc_sdk::TestRecipe {
            build: Vec::new(),
            run: vec!["node".into(), impl_file(entry)],
        }
    }
}

fn stmt_may_return(s: &IrStmt) -> bool {
    match s {
        IrStmt::Return(_) => true,
        IrStmt::If { arms, else_block } => {
            arms.iter().any(|(_, b)| b.iter().any(stmt_may_return))
                || else_block
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_may_return))
        }
        _ => false,
    }
}

fn arith_root(e: &IrExpr) -> bool {
    match &e.kind {
        IrExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul,
            ..
        }
        | IrExprKind::Unary {
            op: UnaryOp::Neg, ..
        } => matches!(e.ty, Ty::Int),
        _ => false,
    }
}

fn tuple_has_int(ty: &Ty) -> bool {
    match ty {
        Ty::Int => true,
        Ty::Tuple(ts) => ts.iter().any(tuple_has_int),
        _ => false,
    }
}

/// `for i = 0 to xs.length - 1` where `xs` is a list.
fn list_len_src<'a>(from: &'a IrExpr, to: &'a IrExpr) -> Option<&'a str> {
    if !matches!(from.kind, IrExprKind::Int(0)) {
        return None;
    }
    let IrExprKind::Binary {
        op: BinaryOp::Sub,
        lhs,
        rhs,
    } = &to.kind
    else {
        return None;
    };
    if !matches!(rhs.kind, IrExprKind::Int(1)) {
        return None;
    }
    let IrExprKind::Builtin {
        builtin: Builtin::ListLength,
        args,
    } = &lhs.kind
    else {
        return None;
    };
    let a = args.first()?;
    if !matches!(a.ty, Ty::List(_)) {
        return None;
    }
    match &a.kind {
        IrExprKind::Local(n) => Some(n.as_str()),
        _ => None,
    }
}

fn uses_index_value(stmts: &[IrStmt], index: &str) -> bool {
    fn expr(e: &IrExpr, index: &str) -> bool {
        match &e.kind {
            IrExprKind::Local(n) if n == index => true,
            IrExprKind::Index { recv, index: ix } => {
                expr(recv, index)
                    || match &ix.kind {
                        IrExprKind::Local(n) if n == index => false,
                        _ => expr(ix, index),
                    }
            }
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) => xs.iter().any(|x| expr(x, index)),
            IrExprKind::CallFunc { args, .. }
            | IrExprKind::Builtin { args, .. }
            | IrExprKind::NewRecord { args, .. }
            | IrExprKind::NewVariant { args, .. }
            | IrExprKind::MutBuiltin { args, .. } => args.iter().any(|x| expr(x, index)),
            IrExprKind::CallValue { callee, args } => {
                expr(callee, index) || args.iter().any(|x| expr(x, index))
            }
            IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
                expr(recv, index)
            }
            IrExprKind::Binary { lhs, rhs, .. } => expr(lhs, index) || expr(rhs, index),
            _ => false,
        }
    }
    fn stmt(s: &IrStmt, index: &str) -> bool {
        match s {
            IrStmt::Assign { value, .. }
            | IrStmt::TupleAssign { value, .. }
            | IrStmt::Expr(value)
            | IrStmt::Return(Some(value))
            | IrStmt::Assert { cond: value, .. } => expr(value, index),
            IrStmt::If { arms, else_block } => {
                arms.iter().any(|(c, b)| expr(c, index) || b.iter().any(|s| stmt(s, index)))
                    || else_block
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|s| stmt(s, index)))
            }
            IrStmt::While { cond, body } => {
                expr(cond, index) || body.iter().any(|s| stmt(s, index))
            }
            IrStmt::ForRange { from, to, body, .. } => {
                expr(from, index) || expr(to, index) || body.iter().any(|s| stmt(s, index))
            }
            _ => false,
        }
    }
    stmts.iter().any(|s| stmt(s, index))
}

fn place_matches_arg(place: &Place, arg: &IrExpr) -> bool {
    match (place, &arg.kind) {
        (Place::Var(v), IrExprKind::Local(n)) => v == n,
        (Place::Field { base, name, .. }, IrExprKind::GetField { recv, name: field }) => {
            name == field
                && matches!(
                    (&**base, &recv.kind),
                    (Place::Var(b), IrExprKind::Local(r)) if b == r
                )
        }
        _ => false,
    }
}

fn local_name(e: &IrExpr) -> Option<&str> {
    match &e.kind {
        IrExprKind::Local(n) => Some(n),
        _ => None,
    }
}

fn expr_mentions_local(e: &IrExpr, name: &str) -> bool {
    let mut hit = false;
    fn walk(e: &IrExpr, name: &str, hit: &mut bool) {
        if matches!(&e.kind, IrExprKind::Local(n) if n == name) {
            *hit = true;
        }
        match &e.kind {
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) => {
                xs.iter().for_each(|x| walk(x, name, hit))
            }
            IrExprKind::CallFunc { args, .. }
            | IrExprKind::Builtin { args, .. }
            | IrExprKind::NewRecord { args, .. }
            | IrExprKind::NewVariant { args, .. }
            | IrExprKind::MutBuiltin { args, .. } => args.iter().for_each(|x| walk(x, name, hit)),
            IrExprKind::CallValue { callee, args } => {
                walk(callee, name, hit);
                args.iter().for_each(|x| walk(x, name, hit));
            }
            IrExprKind::GetField { recv, .. } | IrExprKind::Unary { operand: recv, .. } => {
                walk(recv, name, hit)
            }
            IrExprKind::Index { recv, index } => {
                walk(recv, name, hit);
                walk(index, name, hit);
            }
            IrExprKind::Binary { lhs, rhs, .. } => {
                walk(lhs, name, hit);
                walk(rhs, name, hit);
            }
            _ => {}
        }
    }
    walk(e, name, &mut hit);
    hit
}

fn fold_i64(e: &IrExpr) -> Option<i64> {
    if !matches!(e.ty, Ty::Int) && !matches!(e.kind, IrExprKind::Int(_)) {
        return None;
    }
    match &e.kind {
        IrExprKind::Int(v) => Some(*v),
        IrExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => fold_i64(operand).and_then(|v| v.checked_neg()),
        IrExprKind::Binary { op, lhs, rhs } => {
            let (l, r) = (fold_i64(lhs)?, fold_i64(rhs)?);
            match op {
                BinaryOp::Add => l.checked_add(r),
                BinaryOp::Sub => l.checked_sub(r),
                BinaryOp::Mul => l.checked_mul(r),
                _ => None,
            }
        }
        _ => None,
    }
}

const SAFE_MIN: &str = "-9007199254740991n";
const SAFE_MAX: &str = "9007199254740991n";

fn safe_i64(v: i64) -> bool {
    const MAX: i64 = (1_i64 << 53) - 1;
    (-MAX..=MAX).contains(&v)
}

fn render_i64(v: i64) -> String {
    if safe_i64(v) {
        if v < 0 {
            format!("({v})")
        } else {
            format!("{v}")
        }
    } else {
        format!("{v}n")
    }
}

fn return_expr(s: &IrStmt) -> Option<&IrExpr> {
    match s {
        IrStmt::Return(v) => v.as_ref(),
        _ => None,
    }
}

/// Index of a trailing `if c { return }` chain, if the statements before it
/// cannot themselves return.
fn ladder_at(stmts: &[IrStmt]) -> Option<usize> {
    let end = stmts.iter().rposition(|s| matches!(s, IrStmt::Return(_)))?;
    if end + 1 != stmts.len() || end == 0 {
        return None;
    }
    let mut i = end;
    while i > 0 && is_ladder_if(&stmts[i - 1]) {
        i -= 1;
    }
    if i == end || stmts[..i].iter().any(stmt_may_return) {
        return None;
    }
    Some(i)
}

fn is_ladder_if(s: &IrStmt) -> bool {
    matches!(
        s,
        IrStmt::If {
            arms,
            else_block: None,
        } if arms.len() == 1 && arm_always_returns(&arms[0].1)
    )
}

fn arm_always_returns(stmts: &[IrStmt]) -> bool {
    stmts.iter().any(|s| match s {
        IrStmt::Return(_) => true,
        IrStmt::If { arms, else_block } => {
            else_block.as_ref().is_some_and(|b| arm_always_returns(b))
                && arms.iter().all(|(_, b)| arm_always_returns(b))
        }
        _ => false,
    })
}

fn returns_structured(stmts: &[IrStmt]) -> bool {
    if ladder_at(stmts).is_some() {
        return true;
    }
    match stmts.split_last() {
        Some((IrStmt::Return(_), prefix)) => !prefix.iter().any(stmt_may_return),
        _ => !stmts.iter().any(stmt_may_return),
    }
}

