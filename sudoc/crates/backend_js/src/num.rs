//! Which `sudo int` values the JS backend may emit as a `number`.
//!
//! A value is a `number` only when every integer it can hold, and every
//! arithmetic result that produces it in number space, lies in
//! `[-(2^53-1), 2^53-1]`. Those are exactly the integers IEEE-754 binary64
//! represents with no rounding. Anything else stays `BigInt`, including a
//! product of two 32-bit factors (the product itself exceeds 2^53).
//!
//! JS bitwise operators coerce to Int32, so this pass never selects them.
//! Shifts and masks are not IR operations; a `mod` by a constant is the
//! bound the IR actually has, and division/modulo of numbers go through
//! floor helpers rather than `| 0` or `>>`.
//!
//! The pass runs once over the program. Emission only consults it.

use std::collections::HashMap;

use sudoc_ir::{
    BinaryOp, Builtin, IrConst, IrExpr, IrExprKind, IrFunc, IrModule, IrPattern, IrStmt, Place, Ty,
    UnaryOp,
};

/// Largest integer magnitude binary64 represents exactly (`2^53 - 1`).
pub(crate) const MIN_SAFE: i128 = -((1i128 << 53) - 1);
pub(crate) const MAX_SAFE: i128 = (1i128 << 53) - 1;

const I64_MIN_I: i128 = i64::MIN as i128;
const I64_MAX_I: i128 = i64::MAX as i128;

/// Inclusive interval. `lo > hi` is bottom (not yet seen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Iv {
    pub lo: i128,
    pub hi: i128,
}

impl Iv {
    fn empty() -> Self {
        Iv { lo: 1, hi: 0 }
    }

    fn full() -> Self {
        Iv {
            lo: I64_MIN_I,
            hi: I64_MAX_I,
        }
    }

    fn from_i64(n: i64) -> Self {
        let n = n as i128;
        Iv { lo: n, hi: n }
    }

    fn is_empty(self) -> bool {
        self.lo > self.hi
    }

    pub(crate) fn fits_number(self) -> bool {
        !self.is_empty() && self.lo >= MIN_SAFE && self.hi <= MAX_SAFE
    }

    /// Divisor cannot be zero, so a number division needs no zero check
    /// beyond what the range already proves.
    pub(crate) fn excludes_zero(self) -> bool {
        !self.is_empty() && (self.hi < 0 || self.lo > 0)
    }

    pub(crate) fn non_negative(self) -> bool {
        !self.is_empty() && self.lo >= 0
    }

    pub(crate) fn contains_zero(self) -> bool {
        !self.is_empty() && self.lo <= 0 && self.hi >= 0
    }

    pub(crate) fn contains_neg(self) -> bool {
        !self.is_empty() && self.lo < 0
    }

    /// `0 * -1` is `-0` in JS. BigInt has no negative zero, so a number
    /// product needs a `|| 0` when this pairing is possible.
    pub(crate) fn product_can_be_neg_zero(self, other: Iv) -> bool {
        (self.contains_zero() && other.contains_neg())
            || (other.contains_zero() && self.contains_neg())
    }
}

/// Range that is too wide to be a safe integer. Used when the real bound
/// does not fit in `i128`; it must not be a wrapped small interval.
fn overflowed() -> Iv {
    Iv::full()
}

fn join(old: Iv, new: Iv, widen: bool) -> Iv {
    if new.is_empty() {
        return old;
    }
    if old.is_empty() {
        return new;
    }
    let mut lo = old.lo.min(new.lo);
    let mut hi = old.hi.max(new.hi);
    if widen {
        if lo < old.lo {
            lo = I64_MIN_I;
        }
        if hi > old.hi {
            hi = I64_MAX_I;
        }
    }
    Iv { lo, hi }
}

fn add_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    match (a.lo.checked_add(b.lo), a.hi.checked_add(b.hi)) {
        (Some(lo), Some(hi)) => Iv { lo, hi },
        _ => overflowed(),
    }
}

fn sub_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    match (a.lo.checked_sub(b.hi), a.hi.checked_sub(b.lo)) {
        (Some(lo), Some(hi)) => Iv { lo, hi },
        _ => overflowed(),
    }
}

fn mul_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    let mut lo = i128::MAX;
    let mut hi = i128::MIN;
    for x in [a.lo, a.hi] {
        for y in [b.lo, b.hi] {
            let Some(p) = x.checked_mul(y) else {
                return overflowed();
            };
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    Iv { lo, hi }
}

fn neg_iv(a: Iv) -> Iv {
    if a.is_empty() {
        return a;
    }
    match (a.hi.checked_neg(), a.lo.checked_neg()) {
        (Some(lo), Some(hi)) => Iv { lo, hi },
        _ => overflowed(),
    }
}

fn abs_iv(a: Iv) -> Iv {
    if a.is_empty() {
        return a;
    }
    // `abs(i64::MIN)` traps. Keep the whole i64 span so the emitter
    // stays on the trapping BigInt helper.
    if a.lo == I64_MIN_I {
        return Iv::full();
    }
    let (Some(lo_abs), Some(hi_abs)) = (a.lo.checked_abs(), a.hi.checked_abs()) else {
        return overflowed();
    };
    let m = lo_abs.max(hi_abs);
    if a.lo < 0 && a.hi > 0 {
        Iv { lo: 0, hi: m }
    } else if a.hi <= 0 {
        Iv {
            lo: -a.hi,
            hi: -a.lo,
        }
    } else {
        a
    }
}

/// Floor-div range. Zero divisors trap, so they are not part of the value.
fn div_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    if b.lo == 0 && b.hi == 0 {
        return Iv::empty();
    }
    // `|floor(a/b)| <= |a|` whenever `|b| >= 1`.
    let (Some(lo_abs), Some(hi_abs)) = (a.lo.checked_abs(), a.hi.checked_abs()) else {
        return overflowed();
    };
    let m = lo_abs.max(hi_abs);
    let Some(lo) = m.checked_neg() else {
        return overflowed();
    };
    Iv { lo, hi: m }
}

/// Floor-mod range. Sign follows the divisor.
fn mod_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    if b.lo > 0 {
        let Some(hi) = b.hi.checked_sub(1) else {
            return overflowed();
        };
        return Iv { lo: 0, hi };
    }
    if b.hi < 0 {
        let Some(lo) = b.lo.checked_add(1) else {
            return overflowed();
        };
        return Iv { lo, hi: 0 };
    }
    if b.lo == 0 && b.hi == 0 {
        return Iv::empty();
    }
    let mag = b.lo.abs().max(b.hi.abs());
    if mag <= 1 {
        return Iv { lo: 0, hi: 0 };
    }
    // Both signs (and possibly zero). Remainder magnitude is `< |b|`.
    let _ = a;
    Iv {
        lo: 1 - mag,
        hi: mag - 1,
    }
}

fn min_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    Iv {
        lo: a.lo.min(b.lo),
        hi: a.hi.min(b.hi),
    }
}

fn max_iv(a: Iv, b: Iv) -> Iv {
    if a.is_empty() || b.is_empty() {
        return Iv::empty();
    }
    Iv {
        lo: a.lo.max(b.lo),
        hi: a.hi.max(b.hi),
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct FuncKey {
    pub module: String,
    pub name: String,
}

impl FuncKey {
    fn func(module: &str, name: &str) -> Self {
        FuncKey {
            module: module.to_string(),
            name: name.to_string(),
        }
    }

    fn test(module: &str, name: &str) -> Self {
        FuncKey {
            module: module.to_string(),
            name: format!("#test:{name}"),
        }
    }
}

/// Solved ranges for one program.
pub(crate) struct NumFacts {
    modules: Vec<IrModule>,
    slots: HashMap<String, Iv>,
}

impl NumFacts {
    pub(crate) fn analyze(modules: &[IrModule]) -> Self {
        let mut facts = NumFacts {
            modules: modules.to_vec(),
            slots: HashMap::new(),
        };
        facts.seed_params();
        if !facts.fixpoint(modules) {
            return facts.bail_full();
        }
        // Tuple components that never acquired a range (a for-in element,
        // an untracked local) are the full i64 span, not "unassigned".
        // Propagate that so later arithmetic is not stored as a number.
        if facts.poison_unknown_components() && !facts.fixpoint(modules) {
            return facts.bail_full();
        }
        facts
    }

    /// Iterate transfer to a fixed point. Widening after a few rounds
    /// keeps the lattice finite. `false` means it did not converge; the
    /// caller must drop every range.
    fn fixpoint(&mut self, modules: &[IrModule]) -> bool {
        for iter in 0..32 {
            let widen = iter >= 8;
            let before = self.slots.clone();
            for m in modules {
                for c in &m.consts {
                    self.transfer_const(&m.name, c, widen);
                }
                for f in &m.funcs {
                    let key = FuncKey::func(&m.name, &f.name);
                    self.transfer_stmts(&m.name, &key, &f.body, widen);
                }
                for t in &m.tests {
                    let key = FuncKey::test(&m.name, &t.name);
                    self.transfer_stmts(&m.name, &key, &t.body, widen);
                }
            }
            if self.slots == before {
                return true;
            }
        }
        false
    }

    fn bail_full(mut self) -> Self {
        // A range that is still growing is not a proof. Treat every
        // slot as the full i64 span so emission stays on BigInt.
        for v in self.slots.values_mut() {
            *v = Iv::full();
        }
        self
    }

    /// Join the full i64 span into int locals bound from a tuple component
    /// that still has no range. Returns whether any slot changed.
    fn poison_unknown_components(&mut self) -> bool {
        let modules = self.modules.clone();
        let mut changed = false;
        for m in &modules {
            for f in &m.funcs {
                let key = FuncKey::func(&m.name, &f.name);
                changed |= self.poison_stmts(&m.name, &key, &f.body);
            }
            for t in &m.tests {
                let key = FuncKey::test(&m.name, &t.name);
                changed |= self.poison_stmts(&m.name, &key, &t.body);
            }
        }
        changed
    }

    fn poison_stmts(&mut self, module: &str, func: &FuncKey, stmts: &[IrStmt]) -> bool {
        let mut changed = false;
        for s in stmts {
            changed |= self.poison_stmt(module, func, s);
        }
        changed
    }

    fn poison_stmt(&mut self, module: &str, func: &FuncKey, s: &IrStmt) -> bool {
        match s {
            IrStmt::TupleAssign { targets, value, .. } => {
                let mut changed = false;
                let n = tuple_len(&value.ty);
                for i in 0..n {
                    if !is_int_comp(value, i) {
                        continue;
                    }
                    let Some(name) = targets.get(i) else {
                        continue;
                    };
                    let known = match &value.kind {
                        IrExprKind::Tuple(xs) => {
                            xs.get(i).is_some_and(|x| !self.query(func, x).is_empty())
                        }
                        IrExprKind::CallFunc { name: callee, .. } => self
                            .resolve_key(module, callee)
                            .is_some_and(|key| !self.slot(&ret_key(&key, i)).is_empty()),
                        IrExprKind::Local(local) => {
                            !self.slot(&part_key(func, local, i)).is_empty()
                        }
                        _ => false,
                    };
                    if !known {
                        let key = local_key(func, name);
                        let old = self.slot(&key);
                        let next = join(old, Iv::full(), false);
                        if next != old {
                            self.slots.insert(key, next);
                            changed = true;
                        }
                    }
                }
                changed
            }
            IrStmt::If { arms, else_block } => {
                let mut changed = false;
                for (_, b) in arms {
                    changed |= self.poison_stmts(module, func, b);
                }
                if let Some(b) = else_block {
                    changed |= self.poison_stmts(module, func, b);
                }
                changed
            }
            IrStmt::While { body, .. }
            | IrStmt::ForRange { body, .. }
            | IrStmt::ForIn { body, .. }
            | IrStmt::ExpectTrap { body, .. } => self.poison_stmts(module, func, body),
            IrStmt::Match { arms, .. } => {
                let mut changed = false;
                for a in arms {
                    changed |= self.poison_stmts(module, func, &a.body);
                }
                changed
            }
            _ => false,
        }
    }

    pub(crate) fn record_field_is_number(&self, rec: &str, field: &str) -> bool {
        self.slot(&record_key(rec, field)).fits_number()
    }

    pub(crate) fn variant_field_is_number(&self, en: &str, variant: &str, field: &str) -> bool {
        self.slot(&variant_key(en, variant, field)).fits_number()
    }

    pub(crate) fn enum_field_is_number(&self, en: &str, field: &str) -> bool {
        self.slot(&enum_field_key(en, field)).fits_number()
    }

    pub(crate) fn part_is_number(&self, func: &FuncKey, local: &str, idx: usize) -> bool {
        self.slot(&part_key(func, local, idx)).fits_number()
    }

    /// Final range of an int expression. Bottom becomes the full i64 span
    /// (no proof, so not a number).
    pub(crate) fn range(&self, func: &FuncKey, e: &IrExpr) -> Iv {
        let iv = self.query(func, e);
        if iv.is_empty() {
            Iv::full()
        } else {
            iv
        }
    }

    pub(crate) fn local_is_number(&self, func: &FuncKey, name: &str) -> bool {
        self.slot(&local_key(func, name)).fits_number()
    }

    pub(crate) fn const_is_number(&self, module: &str, name: &str) -> bool {
        self.const_iv(module, name).fits_number()
    }

    pub(crate) fn ret_is_number(&self, func: &FuncKey, idx: usize) -> bool {
        self.slot(&ret_key(func, idx)).fits_number()
    }

    /// The expression's own code is number arithmetic (not a BigInt op
    /// wrapped in `Number`). Storage that merely *holds* a small value
    /// still counts: a local or field proved small is stored as a number.
    pub(crate) fn computes_as_number(&self, func: &FuncKey, e: &IrExpr) -> bool {
        if !is_int(&e.ty) {
            return false;
        }
        match &e.kind {
            IrExprKind::Int(v) => Iv::from_i64(*v).fits_number(),
            IrExprKind::Local(n) => self.local_is_number(func, n),
            IrExprKind::Const(n) => self.const_iv_named(func, n).fits_number(),
            IrExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => self.range(func, operand).fits_number() && self.range(func, e).fits_number(),
            IrExprKind::Binary {
                op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod,
                lhs,
                rhs,
            } => {
                // Children may be BigInt computations (a `mod` of an
                // unbounded int). If their *values* fit, convert and do
                // this operation in number space. A product of two 32-bit
                // residues does not fit, so that multiply stays BigInt.
                self.range(func, lhs).fits_number()
                    && self.range(func, rhs).fits_number()
                    && self.range(func, e).fits_number()
            }
            IrExprKind::GetField { recv, name } => self.field_read_is_number(&recv.ty, name),
            IrExprKind::CallFunc { name, .. } => {
                let Some(key) = self.resolve_key(&func.module, name) else {
                    return false;
                };
                self.ret_is_number(&key, 0)
            }
            IrExprKind::Builtin {
                builtin: Builtin::AbsInt | Builtin::MinInt | Builtin::MaxInt,
                args,
                ..
            } => {
                args.iter().all(|a| self.range(func, a).fits_number())
                    && self.range(func, e).fits_number()
            }
            _ => false,
        }
    }

    fn field_read_is_number(&self, ty: &Ty, field: &str) -> bool {
        match ty {
            Ty::Record(n) => self.record_field_is_number(n, field),
            Ty::Enum(n) => self.slot(&enum_field_key(n, field)).fits_number(),
            _ => false,
        }
    }

    fn query(&self, func: &FuncKey, e: &IrExpr) -> Iv {
        match &e.kind {
            IrExprKind::Int(v) => Iv::from_i64(*v),
            IrExprKind::Local(n) => self.slot(&local_key(func, n)),
            IrExprKind::Const(n) => self.const_iv_named(func, n),
            IrExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } if is_int(&e.ty) => neg_iv(self.query(func, operand)),
            IrExprKind::Binary { op, lhs, rhs } if is_int(&lhs.ty) => {
                let l = self.query(func, lhs);
                let r = self.query(func, rhs);
                match op {
                    BinaryOp::Add => add_iv(l, r),
                    BinaryOp::Sub => sub_iv(l, r),
                    BinaryOp::Mul => mul_iv(l, r),
                    BinaryOp::Div => div_iv(l, r),
                    BinaryOp::Mod => mod_iv(l, r),
                    _ => Iv::empty(),
                }
            }
            IrExprKind::GetField { recv, name } => self.field_iv(&recv.ty, name),
            IrExprKind::CallFunc { name, .. } if is_int(&e.ty) => {
                let Some(key) = self.resolve_key(&func.module, name) else {
                    return Iv::full();
                };
                let iv = self.slot(&ret_key(&key, 0));
                if iv.is_empty() {
                    Iv::empty()
                } else {
                    iv
                }
            }
            IrExprKind::Index { recv, .. } if is_int(&e.ty) => self.elem_iv(func, recv),
            IrExprKind::Builtin { builtin, args } => match builtin {
                Builtin::AbsInt => abs_iv(self.query(func, &args[0])),
                Builtin::MinInt => min_iv(self.query(func, &args[0]), self.query(func, &args[1])),
                Builtin::MaxInt => max_iv(self.query(func, &args[0]), self.query(func, &args[1])),
                Builtin::ListLength | Builtin::MapSize | Builtin::SetSize => Iv {
                    lo: 0,
                    hi: I64_MAX_I,
                },
                Builtin::IntOfFloat => Iv::full(),
                _ => Iv::empty(),
            },
            _ => Iv::empty(),
        }
    }

    fn field_iv(&self, ty: &Ty, field: &str) -> Iv {
        match ty {
            Ty::Record(n) => self.slot(&record_key(n, field)),
            Ty::Enum(n) => self.slot(&enum_field_key(n, field)),
            _ => Iv::empty(),
        }
    }

    fn elem_iv(&self, func: &FuncKey, recv: &IrExpr) -> Iv {
        match &recv.kind {
            IrExprKind::Const(n) => {
                let iv = self.const_list_iv_named(func, n);
                if !iv.is_empty() {
                    iv
                } else if self.const_elem_is_int(func, n) {
                    // Not transferred yet. Bottom, so a later pass can
                    // still record the real element range. `full` here
                    // would stick and force BigInt.
                    Iv::empty()
                } else {
                    Iv::full()
                }
            }
            IrExprKind::List(xs) => {
                let mut acc = Iv::empty();
                let mut any = false;
                for x in xs {
                    if is_int(&x.ty) {
                        any = true;
                        acc = join(acc, self.query(func, x), false);
                    }
                }
                if !any || acc.is_empty() {
                    Iv::full()
                } else {
                    acc
                }
            }
            IrExprKind::Text(cs) => {
                let mut acc = Iv::empty();
                for c in cs {
                    acc = join(acc, Iv::from_i64(*c), false);
                }
                if acc.is_empty() {
                    Iv::full()
                } else {
                    acc
                }
            }
            _ => Iv::full(),
        }
    }

    fn const_iv(&self, module: &str, name: &str) -> Iv {
        self.slot(&const_key(module, name))
    }

    fn const_iv_named(&self, func: &FuncKey, name: &str) -> Iv {
        let (m, n) = self.split_name(&func.module, name);
        self.const_iv(&m, &n)
    }

    fn const_list_iv_named(&self, func: &FuncKey, name: &str) -> Iv {
        let (m, n) = self.split_name(&func.module, name);
        self.slot(&const_list_key(&m, &n))
    }

    fn const_elem_is_int(&self, func: &FuncKey, name: &str) -> bool {
        let (m, n) = self.split_name(&func.module, name);
        let Some(module) = self.modules.iter().find(|x| x.name == m) else {
            return false;
        };
        let Some(c) = module.consts.iter().find(|c| c.name == n) else {
            return false;
        };
        match &c.ty {
            Ty::List(t) if is_int(t) => true,
            _ => matches!(c.value.kind, IrExprKind::Text(_)),
        }
    }

    fn slot(&self, key: &str) -> Iv {
        self.slots.get(key).copied().unwrap_or_else(Iv::empty)
    }

    fn split_name(&self, current: &str, name: &str) -> (String, String) {
        match name.split_once('.') {
            Some((m, n)) => (m.to_string(), n.to_string()),
            None => (current.to_string(), name.to_string()),
        }
    }

    fn resolve_key(&self, current: &str, name: &str) -> Option<FuncKey> {
        let (m, n) = self.split_name(current, name);
        let module = self.modules.iter().find(|x| x.name == m)?;
        module.func(&n)?;
        Some(FuncKey::func(&m, &n))
    }

    fn seed_params(&mut self) {
        // The impl module exports every function, and hosts pass BigInt
        // (a demo calls `face_turn` with `1n`). A parameter's range is the
        // full i64 span; call sites do not narrow it. Locals, fields, and
        // `mod` results still do.
        for m in &self.modules.clone() {
            for f in &m.funcs {
                let key = FuncKey::func(&m.name, &f.name);
                for p in &f.params {
                    if is_int(&p.ty) {
                        self.slots.insert(local_key(&key, &p.name), Iv::full());
                    }
                }
            }
        }
    }

    fn transfer_const(&mut self, module: &str, c: &IrConst, widen: bool) {
        let key = FuncKey::func(module, "#const");
        let iv = self.transfer_expr(module, &key, &c.value, widen);
        if is_int(&c.ty) {
            self.join_slot(&const_key(module, &c.name), iv, widen);
        }
        if let IrExprKind::List(xs) = &c.value.kind {
            if xs.iter().all(|x| is_int(&x.ty)) {
                let mut acc = Iv::empty();
                for x in xs {
                    acc = join(acc, self.transfer_expr(module, &key, x, widen), false);
                }
                if !acc.is_empty() {
                    self.join_slot(&const_list_key(module, &c.name), acc, widen);
                }
            }
        }
        if let IrExprKind::Text(cs) = &c.value.kind {
            let mut acc = Iv::empty();
            for cp in cs {
                acc = join(acc, Iv::from_i64(*cp), false);
            }
            if !acc.is_empty() {
                self.join_slot(&const_list_key(module, &c.name), acc, widen);
            }
        }
    }

    fn transfer_stmts(&mut self, module: &str, func: &FuncKey, stmts: &[IrStmt], widen: bool) {
        for s in stmts {
            self.transfer_stmt(module, func, s, widen);
        }
    }

    fn transfer_stmt(&mut self, module: &str, func: &FuncKey, s: &IrStmt, widen: bool) {
        match s {
            IrStmt::Assign { target, value, .. } => {
                self.note_tuple_local(func, target, value, widen);
                if let Place::Var(n) = target {
                    if is_int(&value.ty) {
                        let iv = self.transfer_expr(module, func, value, widen);
                        self.join_slot(&local_key(func, n), iv, widen);
                    } else {
                        self.transfer_expr(module, func, value, widen);
                    }
                } else if let Place::Field { base_ty, name, .. } = target {
                    if is_int(&value.ty) {
                        let iv = self.transfer_expr(module, func, value, widen);
                        self.join_field(base_ty, name, iv, widen);
                    } else {
                        self.transfer_expr(module, func, value, widen);
                    }
                } else {
                    self.transfer_expr(module, func, value, widen);
                }
            }
            IrStmt::TupleAssign { targets, value, .. } => {
                let comps = self.tuple_comps(module, func, value, widen);
                for (i, name) in targets.iter().enumerate() {
                    if let Ty::Tuple(ts) = &value.ty {
                        if let Some(t) = ts.get(i) {
                            if is_tuple(t) {
                                self.copy_tuple_component(func, name, value, i);
                            }
                        }
                    }
                    if !is_int_comp(value, i) {
                        continue;
                    }
                    // Bottom means "not known yet" (a callee return that this
                    // iteration has not filled). Joining the full span here
                    // would stick, so only a concrete range is joined.
                    // Components still bottom after the fixpoint are poisoned
                    // to the full span in a follow-up pass.
                    if let Some(iv) = comps.get(i).copied() {
                        if !iv.is_empty() {
                            self.join_slot(&local_key(func, name), iv, widen);
                        }
                    }
                }
            }
            IrStmt::Expr(e) => {
                self.transfer_expr(module, func, e, widen);
            }
            IrStmt::Return(Some(e)) => {
                if is_int(&e.ty) {
                    let iv = self.transfer_expr(module, func, e, widen);
                    self.join_slot(&ret_key(func, 0), iv, widen);
                } else if is_tuple(&e.ty) {
                    let comps = self.tuple_comps(module, func, e, widen);
                    for (i, iv) in comps.iter().enumerate() {
                        self.join_slot(&ret_key(func, i), *iv, widen);
                    }
                } else {
                    self.transfer_expr(module, func, e, widen);
                }
            }
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    self.transfer_expr(module, func, c, widen);
                    self.transfer_stmts(module, func, b, widen);
                }
                if let Some(b) = else_block {
                    self.transfer_stmts(module, func, b, widen);
                }
            }
            IrStmt::While { cond, body } => {
                self.transfer_expr(module, func, cond, widen);
                self.transfer_stmts(module, func, body, widen);
            }
            IrStmt::ForRange {
                var,
                from,
                to,
                body,
                ..
            } => {
                let a = self.transfer_expr(module, func, from, widen);
                let b = self.transfer_expr(module, func, to, widen);
                self.join_slot(&local_key(func, var), join(a, b, false), widen);
                self.transfer_stmts(module, func, body, widen);
            }
            IrStmt::ForIn { vars, iter, body } => {
                let iv = self.elem_iv(func, iter);
                self.transfer_expr(module, func, iter, widen);
                if vars.len() == 1 && matches!(iter.ty, Ty::List(ref t) if is_int(t)) {
                    self.join_slot(&local_key(func, &vars[0]), iv, widen);
                }
                self.transfer_stmts(module, func, body, widen);
            }
            IrStmt::Match { scrutinee, arms } => {
                self.transfer_expr(module, func, scrutinee, widen);
                for a in arms {
                    if let IrPattern::Variant {
                        enum_name,
                        variant,
                        binders,
                    } = &a.pattern
                    {
                        self.bind_variant(func, enum_name, variant, binders, widen);
                    }
                    self.transfer_stmts(module, func, &a.body, widen);
                }
            }
            IrStmt::Assert { cond, .. } => {
                self.transfer_expr(module, func, cond, widen);
            }
            IrStmt::ExpectTrap { body, .. } => self.transfer_stmts(module, func, body, widen),
        }
    }

    fn note_tuple_local(&mut self, func: &FuncKey, target: &Place, value: &IrExpr, widen: bool) {
        let Place::Var(n) = target else {
            return;
        };
        if !is_tuple(&value.ty) {
            return;
        }
        self.copy_tuple_binding(func, n, value, widen);
    }

    fn copy_tuple_binding(&mut self, func: &FuncKey, local: &str, value: &IrExpr, widen: bool) {
        let mut paths = Vec::new();
        int_leaves(&value.ty, &mut Vec::new(), &mut paths);
        for path in paths {
            let iv = self.int_at_path(func, value, &path);
            if !iv.is_empty() {
                self.join_slot(&nested_part(func, local, &path), iv, widen);
            }
        }
    }

    /// `r0, r1, r2 = m` where `r0` is itself a tuple: copy `m`'s leaves
    /// under component `index` onto `r0`.
    fn copy_tuple_component(&mut self, func: &FuncKey, target: &str, value: &IrExpr, index: usize) {
        let Ty::Tuple(ts) = &value.ty else {
            return;
        };
        let Some(ty) = ts.get(index) else {
            return;
        };
        let mut rel = Vec::new();
        int_leaves(ty, &mut Vec::new(), &mut rel);
        for path in rel {
            let mut abs = vec![index];
            abs.extend_from_slice(&path);
            let iv = self.int_at_path(func, value, &abs);
            if !iv.is_empty() {
                self.join_slot(&nested_part(func, target, &path), iv, false);
            }
        }
    }

    fn tuple_comps(&mut self, module: &str, func: &FuncKey, e: &IrExpr, widen: bool) -> Vec<Iv> {
        let n = tuple_len(&e.ty);
        match &e.kind {
            IrExprKind::Tuple(xs) => xs
                .iter()
                .map(|x| {
                    if is_int(&x.ty) {
                        self.transfer_expr(module, func, x, widen)
                    } else {
                        self.transfer_expr(module, func, x, widen);
                        Iv::empty()
                    }
                })
                .collect(),
            IrExprKind::CallFunc { name, args } => {
                for a in args {
                    self.transfer_expr(module, func, a, widen);
                }
                self.join_args(module, func, name, args, widen);
                let Some(key) = self.resolve_key(module, name) else {
                    return vec![Iv::full(); n];
                };
                (0..n).map(|i| self.slot(&ret_key(&key, i))).collect()
            }
            IrExprKind::Local(name) => (0..n)
                .map(|i| self.slot(&nested_part(func, name, &[i])))
                .collect(),
            _ => {
                self.transfer_expr(module, func, e, widen);
                vec![Iv::empty(); n]
            }
        }
    }

    fn bind_variant(
        &mut self,
        func: &FuncKey,
        enum_name: &str,
        variant: &str,
        binders: &[String],
        widen: bool,
    ) {
        if enum_name == "Option" || enum_name == "Result" {
            // Payload type is not on the built-in enum. No proof → BigInt.
            return;
        }
        let fields: Vec<(String, bool)> = self
            .modules
            .iter()
            .find_map(|m| m.enum_(enum_name))
            .and_then(|en| en.variants.iter().find(|v| v.name == variant))
            .map(|v| {
                v.fields
                    .iter()
                    .map(|f| (f.name.clone(), is_int(&f.ty)))
                    .collect()
            })
            .unwrap_or_default();
        for (i, name) in binders.iter().enumerate() {
            if let Some((fname, true)) = fields.get(i) {
                let iv = self.slot(&variant_key(enum_name, variant, fname));
                self.join_slot(&local_key(func, name), iv, widen);
            }
        }
    }

    fn transfer_expr(&mut self, module: &str, func: &FuncKey, e: &IrExpr, widen: bool) -> Iv {
        match &e.kind {
            IrExprKind::Int(v) => Iv::from_i64(*v),
            IrExprKind::Local(n) => self.slot(&local_key(func, n)),
            IrExprKind::Const(n) => self.const_iv_named(func, n),
            IrExprKind::Float(_)
            | IrExprKind::Bool(_)
            | IrExprKind::Text(_)
            | IrExprKind::FuncRef(_) => Iv::empty(),
            IrExprKind::List(xs) | IrExprKind::Tuple(xs) => {
                for x in xs {
                    self.transfer_expr(module, func, x, widen);
                }
                Iv::empty()
            }
            IrExprKind::CallFunc { name, args } => {
                for a in args {
                    self.transfer_expr(module, func, a, widen);
                }
                self.join_args(module, func, name, args, widen);
                if is_int(&e.ty) {
                    if let Some(key) = self.resolve_key(module, name) {
                        return self.slot(&ret_key(&key, 0));
                    }
                    return Iv::full();
                }
                Iv::empty()
            }
            IrExprKind::CallValue { callee, args } => {
                self.transfer_expr(module, func, callee, widen);
                for a in args {
                    self.transfer_expr(module, func, a, widen);
                }
                if is_int(&e.ty) {
                    Iv::full()
                } else {
                    Iv::empty()
                }
            }
            IrExprKind::NewRecord { name, args } => {
                let fields = self
                    .modules
                    .iter()
                    .find_map(|m| m.record(name))
                    .map(|r| r.fields.clone());
                if let Some(fields) = fields {
                    for (a, f) in args.iter().zip(fields) {
                        let iv = self.transfer_expr(module, func, a, widen);
                        if is_int(&f.ty) {
                            self.join_slot(&record_key(name, &f.name), iv, widen);
                        }
                    }
                } else {
                    for a in args {
                        self.transfer_expr(module, func, a, widen);
                    }
                }
                Iv::empty()
            }
            IrExprKind::NewVariant {
                enum_name,
                variant,
                args,
            } => {
                let fields = self
                    .modules
                    .iter()
                    .find_map(|m| m.enum_(enum_name))
                    .and_then(|en| en.variants.iter().find(|v| v.name == *variant))
                    .map(|v| v.fields.clone());
                if let Some(fields) = fields {
                    for (a, f) in args.iter().zip(fields) {
                        let iv = self.transfer_expr(module, func, a, widen);
                        if is_int(&f.ty) {
                            self.join_slot(&variant_key(enum_name, variant, &f.name), iv, widen);
                            self.join_slot(&enum_field_key(enum_name, &f.name), iv, widen);
                        }
                    }
                } else {
                    for a in args {
                        self.transfer_expr(module, func, a, widen);
                    }
                }
                Iv::empty()
            }
            IrExprKind::Builtin { builtin, args } => {
                let ivs: Vec<Iv> = args
                    .iter()
                    .map(|a| self.transfer_expr(module, func, a, widen))
                    .collect();
                match builtin {
                    Builtin::AbsInt => abs_iv(ivs[0]),
                    Builtin::MinInt => min_iv(ivs[0], ivs[1]),
                    Builtin::MaxInt => max_iv(ivs[0], ivs[1]),
                    Builtin::ListLength | Builtin::MapSize | Builtin::SetSize => Iv {
                        lo: 0,
                        hi: I64_MAX_I,
                    },
                    Builtin::IntOfFloat => Iv::full(),
                    _ => Iv::empty(),
                }
            }
            IrExprKind::MutBuiltin { args, .. } => {
                for a in args {
                    self.transfer_expr(module, func, a, widen);
                }
                Iv::empty()
            }
            IrExprKind::GetField { recv, name } => {
                self.transfer_expr(module, func, recv, widen);
                if is_int(&e.ty) {
                    self.field_iv(&recv.ty, name)
                } else {
                    Iv::empty()
                }
            }
            IrExprKind::Index { recv, index } => {
                self.transfer_expr(module, func, recv, widen);
                self.transfer_expr(module, func, index, widen);
                if is_int(&e.ty) {
                    self.elem_iv(func, recv)
                } else {
                    Iv::empty()
                }
            }
            IrExprKind::Unary { op, operand } => {
                let iv = self.transfer_expr(module, func, operand, widen);
                if *op == UnaryOp::Neg && is_int(&e.ty) {
                    neg_iv(iv)
                } else {
                    Iv::empty()
                }
            }
            IrExprKind::Binary { op, lhs, rhs } => {
                let l = self.transfer_expr(module, func, lhs, widen);
                let r = self.transfer_expr(module, func, rhs, widen);
                if !is_int(&lhs.ty) {
                    return Iv::empty();
                }
                match op {
                    BinaryOp::Add => add_iv(l, r),
                    BinaryOp::Sub => sub_iv(l, r),
                    BinaryOp::Mul => mul_iv(l, r),
                    BinaryOp::Div => div_iv(l, r),
                    BinaryOp::Mod => mod_iv(l, r),
                    _ => Iv::empty(),
                }
            }
        }
    }

    fn join_args(
        &mut self,
        module: &str,
        func: &FuncKey,
        name: &str,
        args: &[IrExpr],
        widen: bool,
    ) {
        let Some(key) = self.resolve_key(module, name) else {
            return;
        };
        let Some(f) = self.func_by_key(&key) else {
            return;
        };
        // (param name, param is int, param is inout)
        let params: Vec<(String, bool, bool)> = f
            .params
            .iter()
            .map(|p| (p.name.clone(), is_int(&p.ty), p.inout))
            .collect();
        for (a, (pname, is_i, inout)) in args.iter().zip(params) {
            if is_i {
                let iv = self.query(func, a);
                self.join_slot(&local_key(&key, &pname), iv, widen);
                // The callee may store a wider int back into an inout.
                // Flow that slot to the caller's local or field, or the
                // caller's range stays at the pre-call value.
                if inout {
                    let back = self.slot(&local_key(&key, &pname));
                    self.flow_int_to_arg(func, a, back, widen);
                }
            } else if is_tuple(&a.ty) {
                self.flow_tuple_parts(func, &key, &pname, a, widen);
            }
        }
    }

    /// Copy every int leaf of a tuple argument into the callee parameter's
    /// component slots, so `a, b = row` inside the callee sees the call site.
    fn flow_tuple_parts(
        &mut self,
        caller: &FuncKey,
        callee: &FuncKey,
        param: &str,
        arg: &IrExpr,
        widen: bool,
    ) {
        let mut paths = Vec::new();
        int_leaves(&arg.ty, &mut Vec::new(), &mut paths);
        for path in paths {
            let iv = self.int_at_path(caller, arg, &path);
            if !iv.is_empty() {
                self.join_slot(&nested_part(callee, param, &path), iv, widen);
            }
        }
    }

    fn int_at_path(&self, func: &FuncKey, e: &IrExpr, path: &[usize]) -> Iv {
        if path.is_empty() {
            return if is_int(&e.ty) {
                self.query(func, e)
            } else {
                Iv::empty()
            };
        }
        match &e.kind {
            IrExprKind::Tuple(xs) => xs
                .get(path[0])
                .map(|x| self.int_at_path(func, x, &path[1..]))
                .unwrap_or_else(Iv::empty),
            IrExprKind::Local(n) => self.slot(&nested_part(func, n, path)),
            IrExprKind::CallFunc { name, .. } if path.len() == 1 && is_int(&e.ty) => self
                .resolve_key(&func.module, name)
                .map(|key| self.slot(&ret_key(&key, path[0])))
                .unwrap_or_else(Iv::empty),
            _ => Iv::empty(),
        }
    }

    fn flow_int_to_arg(&mut self, func: &FuncKey, arg: &IrExpr, iv: Iv, widen: bool) {
        match &arg.kind {
            IrExprKind::Local(n) => self.join_slot(&local_key(func, n), iv, widen),
            IrExprKind::GetField { recv, name } => self.join_field(&recv.ty, name, iv, widen),
            _ => {}
        }
    }

    fn func_by_key(&self, key: &FuncKey) -> Option<&IrFunc> {
        self.modules
            .iter()
            .find(|m| m.name == key.module)?
            .func(&key.name)
    }

    fn join_field(&mut self, ty: &Ty, name: &str, iv: Iv, widen: bool) {
        match ty {
            Ty::Record(n) => self.join_slot(&record_key(n, name), iv, widen),
            Ty::Enum(n) => self.join_slot(&enum_field_key(n, name), iv, widen),
            _ => {}
        }
    }

    fn join_slot(&mut self, key: &str, iv: Iv, widen: bool) {
        let old = self.slot(key);
        let next = join(old, iv, widen);
        if next != old {
            self.slots.insert(key.to_string(), next);
        } else if !old.is_empty() && !self.slots.contains_key(key) {
            self.slots.insert(key.to_string(), old);
        }
    }
}

fn local_key(func: &FuncKey, name: &str) -> String {
    format!("L\0{}\0{}\0{name}", func.module, func.name)
}

fn ret_key(func: &FuncKey, idx: usize) -> String {
    format!("R\0{}\0{}\0{idx}", func.module, func.name)
}

fn part_key(func: &FuncKey, local: &str, idx: usize) -> String {
    nested_part(func, local, &[idx])
}

fn nested_part(func: &FuncKey, local: &str, path: &[usize]) -> String {
    let path = path
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(".");
    format!("P\0{}\0{}\0{local}\0{path}", func.module, func.name)
}

fn int_leaves(ty: &Ty, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    match ty {
        Ty::Int => out.push(prefix.clone()),
        Ty::Tuple(ts) => {
            for (i, t) in ts.iter().enumerate() {
                prefix.push(i);
                int_leaves(t, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

fn record_key(rec: &str, field: &str) -> String {
    format!("F\0{rec}\0{field}")
}

fn variant_key(en: &str, variant: &str, field: &str) -> String {
    format!("V\0{en}\0{variant}\0{field}")
}

fn enum_field_key(en: &str, field: &str) -> String {
    format!("E\0{en}\0{field}")
}

fn const_key(module: &str, name: &str) -> String {
    format!("C\0{module}\0{name}")
}

fn const_list_key(module: &str, name: &str) -> String {
    format!("CL\0{module}\0{name}")
}

fn is_int(ty: &Ty) -> bool {
    matches!(ty, Ty::Int)
}

fn is_tuple(ty: &Ty) -> bool {
    matches!(ty, Ty::Tuple(_))
}

fn tuple_len(ty: &Ty) -> usize {
    match ty {
        Ty::Tuple(ts) => ts.len(),
        _ => 0,
    }
}

fn is_int_comp(e: &IrExpr, i: usize) -> bool {
    match &e.ty {
        Ty::Tuple(ts) => ts.get(i).is_some_and(is_int),
        _ => false,
    }
}
