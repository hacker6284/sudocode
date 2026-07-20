//! Monomorphized type generation: every sudo type used by the module gets a
//! concrete C struct plus `_copy` / `_free` / `_eq` (and `_hash` where used
//! as a Map/Set key), emitted in dependency order with prototypes first so
//! function bodies can appear in any order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrStmt, Place, Ty};

/// Boxing rule (lib.rs docs): enum/Option/Result payload fields of these
/// kinds live behind a heap pointer — this is what makes recursion finite.
pub(crate) fn boxed_in_payload(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Record(_) | Ty::Enum(_) | Ty::Option_(_) | Ty::Result_(..) | Ty::Tuple(_)
    )
}

pub(crate) fn is_scalar(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::Float | Ty::Bool)
}

/// Mangled name — also the typedef name for composite types.
pub(crate) fn mangle(ty: &Ty) -> String {
    match ty {
        Ty::Int => "i64".into(),
        Ty::Float => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::List(e) => format!("List_{}", mangle(e)),
        Ty::Set(e) => format!("Set_{}", mangle(e)),
        Ty::Map(k, v) => format!("Map_{}_{}", mangle(k), mangle(v)),
        Ty::Option_(e) => format!("Opt_{}", mangle(e)),
        Ty::Result_(t, e) => format!("Res_{}_{}", mangle(t), mangle(e)),
        Ty::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(mangle).collect();
            format!("Tup{}_{}", ts.len(), parts.join("_"))
        }
        Ty::Func { params, ret } => {
            let parts: Vec<String> = params.iter().map(mangle).collect();
            let r = ret.as_ref().map(|r| mangle(r)).unwrap_or_else(|| "void".into());
            format!("Fn_{}_to_{r}", parts.join("_"))
        }
        Ty::Record(n) | Ty::Enum(n) => n.clone(),
        Ty::Infer(_) => unreachable!("Infer escaped the checker"),
    }
}

/// The C type name used in declarations.
pub(crate) fn c_type(ty: &Ty) -> String {
    match ty {
        Ty::Int => "int64_t".into(),
        Ty::Float => "double".into(),
        Ty::Bool => "bool".into(),
        _ => mangle(ty),
    }
}

// ---- component operation snippets ------------------------------------------

/// C expression producing an owned copy of `*src` (src: `const T *`).
pub(crate) fn copy_of(ty: &Ty, src: &str) -> String {
    if is_scalar(ty) || matches!(ty, Ty::Func { .. }) {
        format!("*{src}")
    } else {
        format!("{}_copy({src})", mangle(ty))
    }
}

/// C statement freeing `*ptr`, or empty for trivially-copyable types.
pub(crate) fn free_of(ty: &Ty, ptr: &str) -> String {
    if is_scalar(ty) || matches!(ty, Ty::Func { .. }) {
        String::new()
    } else {
        format!("{}_free({ptr});", mangle(ty))
    }
}

pub(crate) fn eq_of(ty: &Ty, a: &str, b: &str) -> String {
    if is_scalar(ty) || matches!(ty, Ty::Func { .. }) {
        format!("(*{a} == *{b})")
    } else {
        format!("{}_eq({a}, {b})", mangle(ty))
    }
}

/// C statement(s) appending the canonical form of `*p` to the detail buffer.
pub(crate) fn canon_of(ty: &Ty, p: &str) -> String {
    match ty {
        Ty::Int => format!("sudo_det_i64(*{p});"),
        Ty::Float => format!("sudo_det_f64(*{p});"),
        Ty::Bool => format!("sudo_det_bool(*{p});"),
        Ty::Func { .. } => "sudo_det_str(\"<func>\");".into(),
        _ => format!("{}_canon({p});", mangle(ty)),
    }
}

pub(crate) fn hash_of(ty: &Ty, p: &str) -> String {
    match ty {
        Ty::Int => format!("sudo_hash_u64((uint64_t)*{p})"),
        Ty::Bool => format!("sudo_hash_u64(*{p} ? 1u : 2u)"),
        _ => format!("{}_hash({p})", mangle(ty)),
    }
}

// ---- type collection --------------------------------------------------------

#[derive(Default)]
pub(crate) struct TypeSet {
    /// All composite types, keyed by mangled name for determinism.
    pub types: BTreeMap<String, Ty>,
    /// Mangled names of types needing `_hash` (Map keys / Set elements,
    /// transitively).
    pub hashed: BTreeSet<String>,
}

impl TypeSet {
    fn add(&mut self, ty: &Ty) {
        // Scalars need no generation; the empty tuple is the checker's
        // internal void sentinel and never materializes as a value.
        if is_scalar(ty) || matches!(ty, Ty::Tuple(ts) if ts.is_empty()) {
            return;
        }
        let name = mangle(ty);
        if self.types.insert(name, ty.clone()).is_some() {
            return; // already collected (with its components)
        }
        match ty {
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => self.add(e),
            Ty::Map(k, v) => {
                self.add(k);
                self.add(v);
            }
            Ty::Result_(t, e) => {
                self.add(t);
                self.add(e);
            }
            Ty::Tuple(ts) => ts.iter().for_each(|t| self.add(t)),
            Ty::Func { params, ret } => {
                params.iter().for_each(|t| self.add(t));
                if let Some(r) = ret {
                    self.add(r);
                }
            }
            _ => {} // records/enums: fields collected from module decls
        }
        if let Ty::Map(k, _) | Ty::Set(k) = ty {
            self.mark_hashed(k);
        }
    }

    fn mark_hashed(&mut self, ty: &Ty) {
        if is_scalar(ty) {
            return;
        }
        if !self.hashed.insert(mangle(ty)) {
            return;
        }
        match ty {
            Ty::List(e) | Ty::Option_(e) => self.mark_hashed(e),
            Ty::Result_(t, e) => {
                self.mark_hashed(t);
                self.mark_hashed(e);
            }
            Ty::Tuple(ts) => ts.iter().for_each(|t| self.mark_hashed(t)),
            _ => {}
        }
    }

    /// Records/enums need their field types marked when the record itself is
    /// hashed; resolved against the module after collection.
    fn close_hashed_over_decls(&mut self, m: &IrModule) {
        loop {
            let mut new: Vec<Ty> = Vec::new();
            for name in self.hashed.clone() {
                if let Some(r) = m.records.iter().find(|r| r.name == name) {
                    for (_, t) in &r.fields {
                        if !is_scalar(t) && !self.hashed.contains(&mangle(t)) {
                            new.push(t.clone());
                        }
                    }
                }
                if let Some(e) = m.enums.iter().find(|e| e.name == name) {
                    for v in &e.variants {
                        for (_, t) in &v.fields {
                            if !is_scalar(t) && !self.hashed.contains(&mangle(t)) {
                                new.push(t.clone());
                            }
                        }
                    }
                }
            }
            if new.is_empty() {
                break;
            }
            for t in new {
                self.mark_hashed(&t);
            }
        }
    }
}

pub(crate) fn collect(m: &IrModule) -> TypeSet {
    let mut set = TypeSet::default();
    for r in &m.records {
        set.add(&Ty::Record(r.name.clone()));
        for (_, t) in &r.fields {
            set.add(t);
        }
    }
    for e in &m.enums {
        set.add(&Ty::Enum(e.name.clone()));
        for v in &e.variants {
            for (_, t) in &v.fields {
                set.add(t);
            }
        }
    }
    for c in &m.consts {
        set.add(&c.ty);
    }
    for f in &m.funcs {
        for p in &f.params {
            set.add(&p.ty);
        }
        if let Some(r) = &f.ret {
            set.add(r);
        }
        walk_stmts(&f.body, &mut set);
    }
    for t in &m.tests {
        walk_stmts(&t.body, &mut set);
    }
    set.close_hashed_over_decls(m);
    set
}

fn walk_stmts(stmts: &[IrStmt], set: &mut TypeSet) {
    for s in stmts {
        match s {
            IrStmt::Assign { target, value, .. } => {
                walk_place(target, set);
                walk_expr(value, set);
            }
            IrStmt::TupleAssign { value, .. } => walk_expr(value, set),
            IrStmt::Expr(e) => walk_expr(e, set),
            IrStmt::If { arms, else_block } => {
                for (c, b) in arms {
                    walk_expr(c, set);
                    walk_stmts(b, set);
                }
                if let Some(b) = else_block {
                    walk_stmts(b, set);
                }
            }
            IrStmt::While { cond, body } => {
                walk_expr(cond, set);
                walk_stmts(body, set);
            }
            IrStmt::ForRange { from, to, body, .. } => {
                walk_expr(from, set);
                walk_expr(to, set);
                walk_stmts(body, set);
            }
            IrStmt::ForIn { iter, body, .. } => {
                walk_expr(iter, set);
                walk_stmts(body, set);
            }
            IrStmt::Match { scrutinee, arms } => {
                walk_expr(scrutinee, set);
                for a in arms {
                    walk_stmts(&a.body, set);
                }
            }
            IrStmt::Return(Some(e)) => walk_expr(e, set),
            IrStmt::Assert { cond, .. } => walk_expr(cond, set),
            IrStmt::ExpectTrap { body, .. } => walk_stmts(body, set),
            IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
        }
    }
}

fn walk_expr(e: &IrExpr, set: &mut TypeSet) {
    set.add(&e.ty);
    match &e.kind {
        IrExprKind::List(xs)
        | IrExprKind::Tuple(xs)
        | IrExprKind::CallFunc { args: xs, .. }
        | IrExprKind::NewRecord { args: xs, .. }
        | IrExprKind::NewVariant { args: xs, .. }
        | IrExprKind::Builtin { args: xs, .. } => xs.iter().for_each(|x| walk_expr(x, set)),
        IrExprKind::CallValue { callee, args } => {
            walk_expr(callee, set);
            args.iter().for_each(|x| walk_expr(x, set));
        }
        IrExprKind::MutBuiltin { recv, recv_ty, args, .. } => {
            set.add(recv_ty);
            walk_place(recv, set);
            args.iter().for_each(|x| walk_expr(x, set));
        }
        IrExprKind::GetField { recv, .. } => walk_expr(recv, set),
        IrExprKind::Index { recv, index } => {
            walk_expr(recv, set);
            walk_expr(index, set);
        }
        IrExprKind::Unary { operand, .. } => walk_expr(operand, set),
        IrExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, set);
            walk_expr(rhs, set);
        }
        _ => {}
    }
}

fn walk_place(p: &Place, set: &mut TypeSet) {
    match p {
        Place::Var(_) => {}
        Place::Index { base, base_ty, index } => {
            set.add(base_ty);
            walk_place(base, set);
            walk_expr(index, set);
        }
        Place::Field { base, base_ty, .. } => {
            set.add(base_ty);
            walk_place(base, set);
        }
    }
}

// ---- emission ---------------------------------------------------------------

/// Topologically ordered mangled names: a type appears after everything its
/// struct definition needs *complete* (pointers only need the up-front
/// typedefs). Cycles are impossible: the checker rejects by-value record
/// cycles and enum payloads that could recurse are boxed.
fn topo_order(set: &TypeSet, m: &IrModule) -> Vec<String> {
    fn complete_deps(ty: &Ty, m: &IrModule, out: &mut Vec<Ty>) {
        match ty {
            // A List's struct holds only a pointer; but if the element is
            // itself an instantiation, its typedef must exist first.
            Ty::List(e) => {
                if !is_scalar(e) && !matches!(**e, Ty::Record(_) | Ty::Enum(_)) {
                    out.push((**e).clone());
                }
            }
            // Map/Set entries hold keys/values by value.
            Ty::Map(k, v) => {
                out.push((**k).clone());
                out.push((**v).clone());
            }
            Ty::Set(e) => out.push((**e).clone()),
            Ty::Option_(e) => {
                if boxed_in_payload(e) {
                    if !matches!(**e, Ty::Record(_) | Ty::Enum(_)) {
                        out.push((**e).clone());
                    }
                } else {
                    out.push((**e).clone());
                }
            }
            Ty::Result_(t, e) => {
                for side in [t, e] {
                    if boxed_in_payload(side) {
                        if !matches!(**side, Ty::Record(_) | Ty::Enum(_)) {
                            out.push((**side).clone());
                        }
                    } else {
                        out.push((**side).clone());
                    }
                }
            }
            Ty::Tuple(ts) => out.extend(ts.iter().cloned()),
            Ty::Record(name) => {
                if let Some(r) = m.records.iter().find(|r| r.name == *name) {
                    out.extend(r.fields.iter().map(|(_, t)| t.clone()));
                }
            }
            Ty::Enum(name) => {
                if let Some(e) = m.enums.iter().find(|e| e.name == *name) {
                    for v in &e.variants {
                        for (_, t) in &v.fields {
                            if boxed_in_payload(t) {
                                if !matches!(t, Ty::Record(_) | Ty::Enum(_)) {
                                    out.push(t.clone());
                                }
                            } else {
                                out.push(t.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        out.retain(|t| !is_scalar(t));
    }

    let mut order = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    fn visit(
        name: &str,
        set: &TypeSet,
        m: &IrModule,
        done: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if done.contains(name) {
            return;
        }
        done.insert(name.to_string());
        let ty = &set.types[name];
        let mut deps = Vec::new();
        complete_deps(ty, m, &mut deps);
        for d in deps {
            let dn = mangle(&d);
            if set.types.contains_key(&dn) {
                visit(&dn, set, m, done, order);
            }
        }
        order.push(name.to_string());
    }
    for name in set.types.keys() {
        visit(name, set, m, &mut done, &mut order);
    }
    order
}

pub(crate) fn emit_types(m: &IrModule, set: &TypeSet, out: &mut String) {
    // Up-front typedefs for all named types (pointers to them work anywhere).
    for r in &m.records {
        let _ = writeln!(out, "typedef struct {0} {0};", r.name);
    }
    for e in &m.enums {
        let _ = writeln!(out, "typedef struct {0} {0};", e.name);
    }
    if !m.records.is_empty() || !m.enums.is_empty() {
        let _ = writeln!(out);
    }

    let order = topo_order(set, m);

    // Struct definitions.
    for name in &order {
        emit_struct(&set.types[name], m, out);
    }
    let _ = writeln!(out);

    // Prototypes, then bodies (bodies may call each other freely).
    let mut protos = String::new();
    let mut bodies = String::new();
    for name in &order {
        emit_ops(&set.types[name], m, set, &mut protos, &mut bodies);
    }
    out.push_str(&protos);
    let _ = writeln!(out);
    out.push_str(&bodies);
}

fn payload_decl(ty: &Ty, field: &str) -> String {
    if boxed_in_payload(ty) {
        format!("{} *{field};", c_type(ty))
    } else {
        format!("{} {field};", c_type(ty))
    }
}

fn emit_struct(ty: &Ty, m: &IrModule, out: &mut String) {
    let n = mangle(ty);
    match ty {
        Ty::List(e) => {
            let _ = writeln!(
                out,
                "typedef struct {{ {} *data; int64_t len; int64_t cap; }} {n};",
                c_type(e)
            );
        }
        Ty::Map(k, v) => {
            let _ = writeln!(
                out,
                "typedef struct {{ struct {n}_entry {{ {} key; {} value; uint8_t state; }} *entries; int64_t size; int64_t cap; int64_t used; }} {n};",
                c_type(k),
                c_type(v)
            );
        }
        Ty::Set(e) => {
            let _ = writeln!(
                out,
                "typedef struct {{ struct {n}_entry {{ {} item; uint8_t state; }} *entries; int64_t size; int64_t cap; int64_t used; }} {n};",
                c_type(e)
            );
        }
        Ty::Option_(e) => {
            let _ = writeln!(out, "typedef struct {{ bool has; {} }} {n};", payload_decl(e, "value"));
        }
        Ty::Result_(t, e) => {
            let _ = writeln!(
                out,
                "typedef struct {{ bool is_ok; {} {} }} {n};",
                payload_decl(t, "ok"),
                payload_decl(e, "err")
            );
        }
        Ty::Tuple(ts) => {
            let fields: Vec<String> =
                ts.iter().enumerate().map(|(i, t)| format!("{} f{i};", c_type(t))).collect();
            let _ = writeln!(out, "typedef struct {{ {} }} {n};", fields.join(" "));
        }
        Ty::Func { params, ret } => {
            let ps: Vec<String> = params.iter().map(c_type).collect();
            let r = ret.as_ref().map(|r| c_type(r)).unwrap_or_else(|| "void".into());
            let args = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
            let _ = writeln!(out, "typedef {r} (*{n})({args});");
        }
        Ty::Record(name) => {
            let r = m.record(name).expect("record exists");
            let fields: Vec<String> =
                r.fields.iter().map(|(f, t)| format!("{} {f};", c_type(t))).collect();
            let _ = writeln!(out, "struct {name} {{ {} }};", fields.join(" "));
        }
        Ty::Enum(name) => {
            let e = m.enum_(name).expect("enum exists");
            let tags: Vec<String> =
                e.variants.iter().map(|v| format!("{name}_{}_TAG", v.name)).collect();
            let _ = writeln!(out, "enum {{ {} }};", tags.join(", "));
            let mut unions = String::new();
            for v in &e.variants {
                if v.fields.is_empty() {
                    continue;
                }
                let fields: Vec<String> =
                    v.fields.iter().map(|(f, t)| payload_decl(t, f)).collect();
                let _ = write!(unions, "struct {{ {} }} {}; ", fields.join(" "), v.name);
            }
            if unions.is_empty() {
                let _ = writeln!(out, "struct {name} {{ int32_t tag; }};");
            } else {
                let _ = writeln!(out, "struct {name} {{ int32_t tag; union {{ {unions}}} as; }};");
            }
        }
        _ => {}
    }
}

/// Copy expression for a payload slot (boxed or inline).
fn payload_copy(ty: &Ty, slot: &str, out: &mut String) -> String {
    if boxed_in_payload(ty) {
        let ct = c_type(ty);
        let m = mangle(ty);
        let _ = writeln!(out, "    {ct} *_p = sudo_alloc(sizeof({ct}));");
        let _ = writeln!(out, "    *_p = {m}_copy({slot});");
        "_p".into()
    } else {
        copy_of(ty, slot)
    }
}

fn emit_ops(ty: &Ty, m: &IrModule, set: &TypeSet, protos: &mut String, bodies: &mut String) {
    let n = mangle(ty);
    let ct = c_type(ty);
    if matches!(ty, Ty::Func { .. }) {
        return; // function pointers copy trivially
    }
    let hash_needed = set.hashed.contains(&n);

    let mut proto = |sig: &str| {
        let _ = writeln!(protos, "static SUDO_UNUSED {sig};");
    };
    macro_rules! body {
        ($($arg:tt)*) => {{ let _ = writeln!(bodies, $($arg)*); }};
    }

    match ty {
        Ty::List(e) => {
            let et = c_type(e);
            let em = mangle(e);
            let _ = em;
            proto(&format!("{ct} {n}_new(void)"));
            proto(&format!("void {n}_push({ct} *v, {et} x)"));
            proto(&format!("{et} *{n}_at(const {ct} *v, int64_t i)"));
            proto(&format!("{et} {n}_pop({ct} *v)"));
            proto(&format!("void {n}_insert({ct} *v, int64_t i, {et} x)"));
            proto(&format!("{et} {n}_remove_at({ct} *v, int64_t i)"));
            proto(&format!("void {n}_swap({ct} *v, int64_t i, int64_t j)"));
            proto(&format!("{ct} {n}_filled(int64_t count, {et} proto)"));
            proto(&format!("{ct} {n}_concat(const {ct} *a, const {ct} *b)"));
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            if is_scalar(e) && !matches!(**e, Ty::Bool) {
                proto(&format!("void {n}_sort({ct} *v)"));
            }
            if matches!(**e, Ty::Int | Ty::Float) {
                proto(&format!("{ct} {n}_from(const {et} *xs, int64_t count)"));
            }
            if hash_needed {
                proto(&format!("uint64_t {n}_hash(const {ct} *v)"));
            }
            proto(&format!("void {n}_canon(const {ct} *v)"));

            body!("static {ct} {n}_new(void) {{ return ({ct}){{NULL, 0, 0}}; }}");
            body!("static void {n}_grow({ct} *v) {{");
            body!("    if (v->len < v->cap) return;");
            body!("    v->cap = v->cap ? v->cap * 2 : 8;");
            body!("    v->data = sudo_realloc(v->data, (size_t)v->cap * sizeof({et}));");
            body!("}}");
            body!("static void {n}_push({ct} *v, {et} x) {{ {n}_grow(v); v->data[v->len++] = x; }}");
            body!("static {et} *{n}_at(const {ct} *v, int64_t i) {{");
            body!("    if (i < 0 || i >= v->len) sudo_trap(SUDO_TRAP_OUT_OF_BOUNDS, 0);");
            body!("    return ({et} *)&v->data[i];");
            body!("}}");
            body!("static {et} {n}_pop({ct} *v) {{");
            body!("    if (v->len == 0) sudo_trap(SUDO_TRAP_OUT_OF_BOUNDS, 0);");
            body!("    return v->data[--v->len];");
            body!("}}");
            body!("static void {n}_insert({ct} *v, int64_t i, {et} x) {{");
            body!("    if (i < 0 || i > v->len) sudo_trap(SUDO_TRAP_OUT_OF_BOUNDS, 0);");
            body!("    {n}_grow(v);");
            body!("    memmove(&v->data[i + 1], &v->data[i], (size_t)(v->len - i) * sizeof({et}));");
            body!("    v->data[i] = x;");
            body!("    v->len++;");
            body!("}}");
            body!("static {et} {n}_remove_at({ct} *v, int64_t i) {{");
            body!("    if (i < 0 || i >= v->len) sudo_trap(SUDO_TRAP_OUT_OF_BOUNDS, 0);");
            body!("    {et} r = v->data[i];");
            body!("    memmove(&v->data[i], &v->data[i + 1], (size_t)(v->len - i - 1) * sizeof({et}));");
            body!("    v->len--;");
            body!("    return r;");
            body!("}}");
            body!("static void {n}_swap({ct} *v, int64_t i, int64_t j) {{");
            body!("    if (i < 0 || i >= v->len || j < 0 || j >= v->len) sudo_trap(SUDO_TRAP_OUT_OF_BOUNDS, 0);");
            body!("    {et} t = v->data[i]; v->data[i] = v->data[j]; v->data[j] = t;");
            body!("}}");
            body!("static {ct} {n}_filled(int64_t count, {et} proto) {{");
            body!("    if (count < 0) sudo_trap(SUDO_TRAP_INVALID_ARG, 0);");
            body!("    {ct} r = {n}_new();");
            body!("    for (int64_t i = 0; i < count; i++) {n}_push(&r, {});", copy_of(e, "&proto"));
            {
                let f = free_of(e, "&proto");
                if !f.is_empty() {
                    body!("    {f}");
                }
            }
            body!("    return r;");
            body!("}}");
            body!("static {ct} {n}_concat(const {ct} *a, const {ct} *b) {{");
            body!("    {ct} r = {n}_new();");
            body!("    for (int64_t i = 0; i < a->len; i++) {n}_push(&r, {});", copy_of(e, "&a->data[i]"));
            body!("    for (int64_t i = 0; i < b->len; i++) {n}_push(&r, {});", copy_of(e, "&b->data[i]"));
            body!("    return r;");
            body!("}}");
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            body!("    {ct} r = {{NULL, v->len, v->len}};");
            body!("    if (v->len) r.data = sudo_alloc((size_t)v->len * sizeof({et}));");
            body!("    for (int64_t i = 0; i < v->len; i++) r.data[i] = {};", copy_of(e, "&v->data[i]"));
            body!("    return r;");
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            {
                let f = free_of(e, "&v->data[i]");
                if !f.is_empty() {
                    body!("    for (int64_t i = 0; i < v->len; i++) {f}");
                }
            }
            body!("    sudo_dealloc(v->data);");
            body!("    v->data = NULL; v->len = v->cap = 0;");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->len != b->len) return false;");
            body!("    for (int64_t i = 0; i < a->len; i++) if (!{}) return false;", eq_of(e, "&a->data[i]", "&b->data[i]"));
            body!("    return true;");
            body!("}}");
            if is_scalar(e) && !matches!(**e, Ty::Bool) {
                let lt = if matches!(**e, Ty::Float) {
                    "sudo_f64_sort_lt(key, v->data[j])".to_string()
                } else {
                    "key < v->data[j]".to_string()
                };
                body!("static void {n}_sort({ct} *v) {{");
                body!("    for (int64_t i = 1; i < v->len; i++) {{");
                body!("        {et} key = v->data[i];");
                body!("        int64_t j = i - 1;");
                body!("        while (j >= 0 && {lt}) {{ v->data[j + 1] = v->data[j]; j--; }}");
                body!("        v->data[j + 1] = key;");
                body!("    }}");
                body!("}}");
            }
            if matches!(**e, Ty::Int | Ty::Float) {
                body!("static {ct} {n}_from(const {et} *xs, int64_t count) {{");
                body!("    {ct} r = {n}_new();");
                body!("    for (int64_t i = 0; i < count; i++) {n}_push(&r, xs[i]);");
                body!("    return r;");
                body!("}}");
            }
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    sudo_det_str(\"[\");");
            body!("    for (int64_t i = 0; i < v->len; i++) {{");
            body!("        if (i) sudo_det_str(\", \");");
            body!("        {}", canon_of(e, "&v->data[i]"));
            body!("    }}");
            body!("    sudo_det_str(\"]\");");
            body!("}}");
            if hash_needed {
                body!("static uint64_t {n}_hash(const {ct} *v) {{");
                body!("    uint64_t h = sudo_hash_u64((uint64_t)v->len);");
                body!("    for (int64_t i = 0; i < v->len; i++) h = sudo_hash_combine(h, {});", hash_of(e, "&v->data[i]"));
                body!("    return h;");
                body!("}}");
            }
        }
        Ty::Map(k, v) => {
            let kt = c_type(k);
            let vt = c_type(v);
            proto(&format!("{ct} {n}_new(void)"));
            proto(&format!("int64_t {n}_find(const {ct} *m, const {kt} *key)"));
            proto(&format!("void {n}_put({ct} *m, {kt} key, {vt} value)"));
            proto(&format!("{vt} *{n}_at(const {ct} *m, const {kt} *key)"));
            proto(&format!("bool {n}_has(const {ct} *m, const {kt} *key)"));
            proto(&format!("bool {n}_delete({ct} *m, const {kt} *key)"));
            proto(&format!("{ct} {n}_copy(const {ct} *m)"));
            proto(&format!("void {n}_free({ct} *m)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            let opt_v = Ty::Option_(Box::new((**v).clone()));
            let has_opt = set.types.contains_key(&mangle(&opt_v));
            if has_opt {
                proto(&format!("{} {n}_get(const {ct} *m, const {kt} *key)", mangle(&opt_v)));
            }
            let list_k = Ty::List(Box::new((**k).clone()));
            if set.types.contains_key(&mangle(&list_k)) {
                proto(&format!("{} {n}_keys(const {ct} *m)", mangle(&list_k)));
            }
            let list_v = Ty::List(Box::new((**v).clone()));
            if set.types.contains_key(&mangle(&list_v)) {
                proto(&format!("{} {n}_values(const {ct} *m)", mangle(&list_v)));
            }
            proto(&format!("void {n}_canon(const {ct} *m)"));

            body!("static void {n}_canon(const {ct} *m) {{");
            body!("    sudo_det_str(\"{{\\\"m\\\": [\");");
            body!("    bool first = true;");
            body!("    for (int64_t i = 0; i < m->cap; i++) {{");
            body!("        if (m->entries[i].state != 1) continue;");
            body!("        if (!first) sudo_det_str(\", \");");
            body!("        first = false;");
            body!("        sudo_det_str(\"[\");");
            body!("        {}", canon_of(k, "&m->entries[i].key"));
            body!("        sudo_det_str(\", \");");
            body!("        {}", canon_of(v, "&m->entries[i].value"));
            body!("        sudo_det_str(\"]\");");
            body!("    }}");
            body!("    sudo_det_str(\"]}}\");");
            body!("}}");
            body!("static {ct} {n}_new(void) {{ return ({ct}){{NULL, 0, 0, 0}}; }}");
            body!("static int64_t {n}_find(const {ct} *m, const {kt} *key) {{");
            body!("    if (m->cap == 0) return -1;");
            body!("    uint64_t h = {};", hash_of(k, "key"));
            body!("    for (int64_t probe = 0; probe < m->cap; probe++) {{");
            body!("        int64_t i = (int64_t)((h + (uint64_t)probe) & (uint64_t)(m->cap - 1));");
            body!("        if (m->entries[i].state == 0) return -1;");
            body!("        if (m->entries[i].state == 1 && {}) return i;", eq_of(k, "&m->entries[i].key", "key"));
            body!("    }}");
            body!("    return -1;");
            body!("}}");
            body!("static void {n}_put_raw({ct} *m, {kt} key, {vt} value);");
            body!("static void {n}_rehash({ct} *m) {{");
            body!("    struct {n}_entry *old = m->entries;");
            body!("    int64_t old_cap = m->cap;");
            body!("    m->cap = m->cap ? m->cap * 2 : 8;");
            body!("    m->entries = sudo_alloc((size_t)m->cap * sizeof(*m->entries));");
            body!("    memset(m->entries, 0, (size_t)m->cap * sizeof(*m->entries));");
            body!("    m->size = 0; m->used = 0;");
            body!("    for (int64_t i = 0; i < old_cap; i++)");
            body!("        if (old[i].state == 1) {n}_put_raw(m, old[i].key, old[i].value);");
            body!("    sudo_dealloc(old);");
            body!("}}");
            body!("static void {n}_put_raw({ct} *m, {kt} key, {vt} value) {{");
            body!("    if (m->cap == 0 || (m->used + 1) * 10 >= m->cap * 7) {n}_rehash(m);");
            body!("    uint64_t h = {};", hash_of(k, "&key"));
            body!("    int64_t slot = -1;");
            body!("    for (int64_t probe = 0; probe < m->cap; probe++) {{");
            body!("        int64_t i = (int64_t)((h + (uint64_t)probe) & (uint64_t)(m->cap - 1));");
            body!("        if (m->entries[i].state == 1 && {}) {{", eq_of(k, "&m->entries[i].key", "&key"));
            {
                let fk = free_of(k, "&key");
                if !fk.is_empty() {
                    body!("            {fk}");
                }
                let fv = free_of(v, "&m->entries[i].value");
                if !fv.is_empty() {
                    body!("            {fv}");
                }
            }
            body!("            m->entries[i].value = value;");
            body!("            return;");
            body!("        }}");
            body!("        if (m->entries[i].state != 1) {{ if (slot < 0) slot = i; if (m->entries[i].state == 0) break; }}");
            body!("    }}");
            body!("    if (m->entries[slot].state == 0) m->used++;");
            body!("    m->entries[slot].key = key;");
            body!("    m->entries[slot].value = value;");
            body!("    m->entries[slot].state = 1;");
            body!("    m->size++;");
            body!("}}");
            body!("static void {n}_put({ct} *m, {kt} key, {vt} value) {{ {n}_put_raw(m, key, value); }}");
            body!("static {vt} *{n}_at(const {ct} *m, const {kt} *key) {{");
            body!("    int64_t i = {n}_find(m, key);");
            body!("    if (i < 0) sudo_trap(SUDO_TRAP_KEY_MISSING, 0);");
            body!("    return ({vt} *)&m->entries[i].value;");
            body!("}}");
            body!("static bool {n}_has(const {ct} *m, const {kt} *key) {{ return {n}_find(m, key) >= 0; }}");
            body!("static bool {n}_delete({ct} *m, const {kt} *key) {{");
            body!("    int64_t i = {n}_find(m, key);");
            body!("    if (i < 0) return false;");
            {
                let fk = free_of(k, "&m->entries[i].key");
                if !fk.is_empty() {
                    body!("    {fk}");
                }
                let fv = free_of(v, "&m->entries[i].value");
                if !fv.is_empty() {
                    body!("    {fv}");
                }
            }
            body!("    m->entries[i].state = 2;");
            body!("    m->size--;");
            body!("    return true;");
            body!("}}");
            body!("static {ct} {n}_copy(const {ct} *m) {{");
            body!("    {ct} r = {{NULL, m->size, m->cap, m->used}};");
            body!("    if (m->cap) {{");
            body!("        r.entries = sudo_alloc((size_t)m->cap * sizeof(*r.entries));");
            body!("        for (int64_t i = 0; i < m->cap; i++) {{");
            body!("            r.entries[i].state = m->entries[i].state;");
            body!("            if (m->entries[i].state == 1) {{");
            body!("                r.entries[i].key = {};", copy_of(k, "&m->entries[i].key"));
            body!("                r.entries[i].value = {};", copy_of(v, "&m->entries[i].value"));
            body!("            }}");
            body!("        }}");
            body!("    }}");
            body!("    return r;");
            body!("}}");
            body!("static void {n}_free({ct} *m) {{");
            body!("    for (int64_t i = 0; i < m->cap; i++) {{");
            body!("        if (m->entries[i].state == 1) {{");
            {
                let fk = free_of(k, "&m->entries[i].key");
                if !fk.is_empty() {
                    body!("            {fk}");
                }
                let fv = free_of(v, "&m->entries[i].value");
                if !fv.is_empty() {
                    body!("            {fv}");
                }
            }
            body!("        }}");
            body!("    }}");
            body!("    sudo_dealloc(m->entries);");
            body!("    m->entries = NULL; m->size = m->cap = m->used = 0;");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->size != b->size) return false;");
            body!("    for (int64_t i = 0; i < a->cap; i++) {{");
            body!("        if (a->entries[i].state != 1) continue;");
            body!("        int64_t j = {n}_find(b, &a->entries[i].key);");
            body!("        if (j < 0 || !{}) return false;", eq_of(v, "&a->entries[i].value", "&b->entries[j].value"));
            body!("    }}");
            body!("    return true;");
            body!("}}");
            if has_opt {
                let on = mangle(&opt_v);
                body!("static {on} {n}_get(const {ct} *m, const {kt} *key) {{");
                body!("    int64_t i = {n}_find(m, key);");
                body!("    if (i < 0) return {on}_none();");
                body!("    return {on}_some({});", copy_of(v, "&m->entries[i].value"));
                body!("}}");
            }
            if set.types.contains_key(&mangle(&list_k)) {
                let ln = mangle(&list_k);
                body!("static {ln} {n}_keys(const {ct} *m) {{");
                body!("    {ln} r = {ln}_new();");
                body!("    for (int64_t i = 0; i < m->cap; i++)");
                body!("        if (m->entries[i].state == 1) {ln}_push(&r, {});", copy_of(k, "&m->entries[i].key"));
                body!("    return r;");
                body!("}}");
            }
            if set.types.contains_key(&mangle(&list_v)) {
                let ln = mangle(&list_v);
                body!("static {ln} {n}_values(const {ct} *m) {{");
                body!("    {ln} r = {ln}_new();");
                body!("    for (int64_t i = 0; i < m->cap; i++)");
                body!("        if (m->entries[i].state == 1) {ln}_push(&r, {});", copy_of(v, "&m->entries[i].value"));
                body!("    return r;");
                body!("}}");
            }
        }
        Ty::Set(e) => {
            let et = c_type(e);
            proto(&format!("{ct} {n}_new(void)"));
            proto(&format!("int64_t {n}_find(const {ct} *s, const {et} *item)"));
            proto(&format!("bool {n}_add({ct} *s, {et} item)"));
            proto(&format!("bool {n}_has(const {ct} *s, const {et} *item)"));
            proto(&format!("bool {n}_remove({ct} *s, const {et} *item)"));
            proto(&format!("{ct} {n}_copy(const {ct} *s)"));
            proto(&format!("void {n}_free({ct} *s)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            let list_e = Ty::List(Box::new((**e).clone()));
            if set.types.contains_key(&mangle(&list_e)) {
                proto(&format!("{} {n}_items(const {ct} *s)", mangle(&list_e)));
            }
            proto(&format!("void {n}_canon(const {ct} *s)"));

            body!("static void {n}_canon(const {ct} *s) {{");
            body!("    sudo_det_str(\"{{\\\"s\\\": [\");");
            body!("    bool first = true;");
            body!("    for (int64_t i = 0; i < s->cap; i++) {{");
            body!("        if (s->entries[i].state != 1) continue;");
            body!("        if (!first) sudo_det_str(\", \");");
            body!("        first = false;");
            body!("        {}", canon_of(e, "&s->entries[i].item"));
            body!("    }}");
            body!("    sudo_det_str(\"]}}\");");
            body!("}}");
            body!("static {ct} {n}_new(void) {{ return ({ct}){{NULL, 0, 0, 0}}; }}");
            body!("static int64_t {n}_find(const {ct} *s, const {et} *item) {{");
            body!("    if (s->cap == 0) return -1;");
            body!("    uint64_t h = {};", hash_of(e, "item"));
            body!("    for (int64_t probe = 0; probe < s->cap; probe++) {{");
            body!("        int64_t i = (int64_t)((h + (uint64_t)probe) & (uint64_t)(s->cap - 1));");
            body!("        if (s->entries[i].state == 0) return -1;");
            body!("        if (s->entries[i].state == 1 && {}) return i;", eq_of(e, "&s->entries[i].item", "item"));
            body!("    }}");
            body!("    return -1;");
            body!("}}");
            body!("static bool {n}_add_raw({ct} *s, {et} item);");
            body!("static void {n}_rehash({ct} *s) {{");
            body!("    struct {n}_entry *old = s->entries;");
            body!("    int64_t old_cap = s->cap;");
            body!("    s->cap = s->cap ? s->cap * 2 : 8;");
            body!("    s->entries = sudo_alloc((size_t)s->cap * sizeof(*s->entries));");
            body!("    memset(s->entries, 0, (size_t)s->cap * sizeof(*s->entries));");
            body!("    s->size = 0; s->used = 0;");
            body!("    for (int64_t i = 0; i < old_cap; i++)");
            body!("        if (old[i].state == 1) (void){n}_add_raw(s, old[i].item);");
            body!("    sudo_dealloc(old);");
            body!("}}");
            body!("static bool {n}_add_raw({ct} *s, {et} item) {{");
            body!("    if (s->cap == 0 || (s->used + 1) * 10 >= s->cap * 7) {n}_rehash(s);");
            body!("    uint64_t h = {};", hash_of(e, "&item"));
            body!("    int64_t slot = -1;");
            body!("    for (int64_t probe = 0; probe < s->cap; probe++) {{");
            body!("        int64_t i = (int64_t)((h + (uint64_t)probe) & (uint64_t)(s->cap - 1));");
            body!("        if (s->entries[i].state == 1 && {}) {{", eq_of(e, "&s->entries[i].item", "&item"));
            {
                let f = free_of(e, "&item");
                if !f.is_empty() {
                    body!("            {f}");
                }
            }
            body!("            return false;");
            body!("        }}");
            body!("        if (s->entries[i].state != 1) {{ if (slot < 0) slot = i; if (s->entries[i].state == 0) break; }}");
            body!("    }}");
            body!("    if (s->entries[slot].state == 0) s->used++;");
            body!("    s->entries[slot].item = item;");
            body!("    s->entries[slot].state = 1;");
            body!("    s->size++;");
            body!("    return true;");
            body!("}}");
            body!("static bool {n}_add({ct} *s, {et} item) {{ return {n}_add_raw(s, item); }}");
            body!("static bool {n}_has(const {ct} *s, const {et} *item) {{ return {n}_find(s, item) >= 0; }}");
            body!("static bool {n}_remove({ct} *s, const {et} *item) {{");
            body!("    int64_t i = {n}_find(s, item);");
            body!("    if (i < 0) return false;");
            {
                let f = free_of(e, "&s->entries[i].item");
                if !f.is_empty() {
                    body!("    {f}");
                }
            }
            body!("    s->entries[i].state = 2;");
            body!("    s->size--;");
            body!("    return true;");
            body!("}}");
            body!("static {ct} {n}_copy(const {ct} *s) {{");
            body!("    {ct} r = {{NULL, s->size, s->cap, s->used}};");
            body!("    if (s->cap) {{");
            body!("        r.entries = sudo_alloc((size_t)s->cap * sizeof(*r.entries));");
            body!("        for (int64_t i = 0; i < s->cap; i++) {{");
            body!("            r.entries[i].state = s->entries[i].state;");
            body!("            if (s->entries[i].state == 1) r.entries[i].item = {};", copy_of(e, "&s->entries[i].item"));
            body!("        }}");
            body!("    }}");
            body!("    return r;");
            body!("}}");
            body!("static void {n}_free({ct} *s) {{");
            {
                let f = free_of(e, "&s->entries[i].item");
                if !f.is_empty() {
                    body!("    for (int64_t i = 0; i < s->cap; i++) if (s->entries[i].state == 1) {f}");
                }
            }
            body!("    sudo_dealloc(s->entries);");
            body!("    s->entries = NULL; s->size = s->cap = s->used = 0;");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->size != b->size) return false;");
            body!("    for (int64_t i = 0; i < a->cap; i++)");
            body!("        if (a->entries[i].state == 1 && {n}_find(b, &a->entries[i].item) < 0) return false;");
            body!("    return true;");
            body!("}}");
            if set.types.contains_key(&mangle(&list_e)) {
                let ln = mangle(&list_e);
                body!("static {ln} {n}_items(const {ct} *s) {{");
                body!("    {ln} r = {ln}_new();");
                body!("    for (int64_t i = 0; i < s->cap; i++)");
                body!("        if (s->entries[i].state == 1) {ln}_push(&r, {});", copy_of(e, "&s->entries[i].item"));
                body!("    return r;");
                body!("}}");
            }
        }
        Ty::Option_(e) => {
            let et = c_type(e);
            let boxed = boxed_in_payload(e);
            proto(&format!("{ct} {n}_some({et} v)"));
            proto(&format!("{ct} {n}_none(void)"));
            proto(&format!("{et} {n}_unwrap(const {ct} *v)"));
            proto(&format!("{et} {n}_get_or(const {ct} *v, {et} dflt)"));
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            if set.hashed.contains(&n) {
                proto(&format!("uint64_t {n}_hash(const {ct} *v)"));
            }
            proto(&format!("void {n}_canon(const {ct} *v)"));

            if boxed {
                body!("static {ct} {n}_some({et} v) {{");
                body!("    {et} *p = sudo_alloc(sizeof({et}));");
                body!("    *p = v;");
                body!("    return ({ct}){{true, p}};");
                body!("}}");
            } else {
                body!("static {ct} {n}_some({et} v) {{ return ({ct}){{true, v}}; }}");
            }
            body!("static {ct} {n}_none(void) {{ {ct} r; memset(&r, 0, sizeof r); r.has = false; return r; }}");
            let payload = if boxed { "v->value".to_string() } else { "&v->value".to_string() };
            body!("static {et} {n}_unwrap(const {ct} *v) {{");
            body!("    if (!v->has) sudo_trap(SUDO_TRAP_UNWRAP_FAILED, 0);");
            body!("    return {};", copy_of(e, &payload));
            body!("}}");
            body!("static {et} {n}_get_or(const {ct} *v, {et} dflt) {{");
            body!("    if (v->has) {{");
            {
                let f = free_of(e, "&dflt");
                if !f.is_empty() {
                    body!("        {f}");
                }
            }
            body!("        return {};", copy_of(e, &payload));
            body!("    }}");
            body!("    return dflt;");
            body!("}}");
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            body!("    if (!v->has) return {n}_none();");
            {
                let mut tmp = String::new();
                let expr = payload_copy(e, &payload, &mut tmp);
                if tmp.is_empty() {
                    body!("    return ({ct}){{true, {expr}}};");
                } else {
                    for line in tmp.lines() {
                        body!("{line}");
                    }
                    body!("    return ({ct}){{true, {expr}}};");
                }
            }
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            body!("    if (v->has) {{");
            if boxed {
                let f = free_of(e, "v->value");
                if !f.is_empty() {
                    body!("        {f}");
                }
                body!("        sudo_dealloc(v->value);");
            } else {
                let f = free_of(e, "&v->value");
                if !f.is_empty() {
                    body!("        {f}");
                }
            }
            body!("    }}");
            body!("    v->has = false;");
            body!("}}");
            let b_payload = if boxed { "b->value".to_string() } else { "&b->value".to_string() };
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->has != b->has) return false;");
            body!("    if (!a->has) return true;");
            body!("    return {};", eq_of(e, &payload.replace("v->", "a->"), &b_payload));
            body!("}}");
            if set.hashed.contains(&n) {
                body!("static uint64_t {n}_hash(const {ct} *v) {{");
                body!("    if (!v->has) return sudo_hash_u64(3);");
                body!("    return sudo_hash_combine(sudo_hash_u64(4), {});", hash_of(e, &payload));
                body!("}}");
            }
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    if (!v->has) {{ sudo_det_str(\"{{\\\"e\\\": \\\"Option.None\\\"}}\"); return; }}");
            body!("    sudo_det_str(\"{{\\\"e\\\": \\\"Option.Some\\\", \\\"v\\\": [\");");
            body!("    {}", canon_of(e, &payload));
            body!("    sudo_det_str(\"]}}\");");
            body!("}}");
        }
        Ty::Result_(t, e) => {
            let tt = c_type(t);
            let et = c_type(e);
            let tb = boxed_in_payload(t);
            let eb = boxed_in_payload(e);
            proto(&format!("{ct} {n}_ok({tt} v)"));
            proto(&format!("{ct} {n}_err({et} v)"));
            proto(&format!("{tt} {n}_unwrap(const {ct} *v)"));
            proto(&format!("{tt} {n}_get_or(const {ct} *v, {tt} dflt)"));
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            proto(&format!("void {n}_canon(const {ct} *v)"));

            let mk = |boxed: bool, ctn: &str, field: &str, flag: &str, tyn: &str| -> String {
                if boxed {
                    format!("static {ct} {n}_{field}({tyn} v) {{ {tyn} *p = sudo_alloc(sizeof({tyn})); *p = v; {ct} r; memset(&r, 0, sizeof r); r.is_ok = {flag}; r.{ctn} = p; return r; }}")
                } else {
                    format!("static {ct} {n}_{field}({tyn} v) {{ {ct} r; memset(&r, 0, sizeof r); r.is_ok = {flag}; r.{ctn} = v; return r; }}")
                }
            };
            body!("{}", mk(tb, "ok", "ok", "true", &tt));
            body!("{}", mk(eb, "err", "err", "false", &et));
            let ok_payload = |var: &str| if tb { format!("{var}->ok") } else { format!("&{var}->ok") };
            let err_payload = |var: &str| if eb { format!("{var}->err") } else { format!("&{var}->err") };
            body!("static {tt} {n}_unwrap(const {ct} *v) {{");
            body!("    if (!v->is_ok) sudo_trap(SUDO_TRAP_UNWRAP_FAILED, 0);");
            body!("    return {};", copy_of(t, &ok_payload("v")));
            body!("}}");
            body!("static {tt} {n}_get_or(const {ct} *v, {tt} dflt) {{");
            body!("    if (v->is_ok) {{");
            {
                let f = free_of(t, "&dflt");
                if !f.is_empty() {
                    body!("        {f}");
                }
            }
            body!("        return {};", copy_of(t, &ok_payload("v")));
            body!("    }}");
            body!("    return dflt;");
            body!("}}");
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            body!("    if (v->is_ok) return {n}_ok({});", copy_of(t, &ok_payload("v")));
            body!("    return {n}_err({});", copy_of(e, &err_payload("v")));
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            body!("    if (v->is_ok) {{");
            if tb {
                let f = free_of(t, "v->ok");
                if !f.is_empty() {
                    body!("        {f}");
                }
                body!("        sudo_dealloc(v->ok);");
            } else {
                let f = free_of(t, "&v->ok");
                if !f.is_empty() {
                    body!("        {f}");
                }
            }
            body!("    }} else {{");
            if eb {
                let f = free_of(e, "v->err");
                if !f.is_empty() {
                    body!("        {f}");
                }
                body!("        sudo_dealloc(v->err);");
            } else {
                let f = free_of(e, "&v->err");
                if !f.is_empty() {
                    body!("        {f}");
                }
            }
            body!("    }}");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->is_ok != b->is_ok) return false;");
            body!("    if (a->is_ok) return {};", eq_of(t, &ok_payload("a"), &ok_payload("b")));
            body!("    return {};", eq_of(e, &err_payload("a"), &err_payload("b")));
            body!("}}");
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    if (v->is_ok) {{");
            body!("        sudo_det_str(\"{{\\\"e\\\": \\\"Result.Ok\\\", \\\"v\\\": [\");");
            body!("        {}", canon_of(t, &ok_payload("v")));
            body!("    }} else {{");
            body!("        sudo_det_str(\"{{\\\"e\\\": \\\"Result.Err\\\", \\\"v\\\": [\");");
            body!("        {}", canon_of(e, &err_payload("v")));
            body!("    }}");
            body!("    sudo_det_str(\"]}}\");");
            body!("}}");
        }
        Ty::Tuple(ts) => {
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            if set.hashed.contains(&n) {
                proto(&format!("uint64_t {n}_hash(const {ct} *v)"));
            }
            proto(&format!("void {n}_canon(const {ct} *v)"));
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            let fields: Vec<String> = ts
                .iter()
                .enumerate()
                .map(|(i, t)| copy_of(t, &format!("&v->f{i}")))
                .collect();
            body!("    return ({ct}){{ {} }};", fields.join(", "));
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            for (i, t) in ts.iter().enumerate() {
                let f = free_of(t, &format!("&v->f{i}"));
                if !f.is_empty() {
                    body!("    {f}");
                }
            }
            body!("    (void)v;");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            let eqs: Vec<String> = ts
                .iter()
                .enumerate()
                .map(|(i, t)| eq_of(t, &format!("&a->f{i}"), &format!("&b->f{i}")))
                .collect();
            body!("    return {};", eqs.join(" && "));
            body!("}}");
            if set.hashed.contains(&n) {
                body!("static uint64_t {n}_hash(const {ct} *v) {{");
                body!("    uint64_t h = sudo_hash_u64({});", ts.len());
                for (i, t) in ts.iter().enumerate() {
                    body!("    h = sudo_hash_combine(h, {});", hash_of(t, &format!("&v->f{i}")));
                }
                body!("    return h;");
                body!("}}");
            }
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    sudo_det_str(\"[\");");
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    body!("    sudo_det_str(\", \");");
                }
                body!("    {}", canon_of(t, &format!("&v->f{i}")));
            }
            body!("    sudo_det_str(\"]\");");
            body!("}}");
        }
        Ty::Record(name) => {
            let r = m.record(name).expect("record exists").clone();
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            if set.hashed.contains(&n) {
                proto(&format!("uint64_t {n}_hash(const {ct} *v)"));
            }
            proto(&format!("void {n}_canon(const {ct} *v)"));
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            let fields: Vec<String> = r
                .fields
                .iter()
                .map(|(f, t)| format!(".{f} = {}", copy_of(t, &format!("&v->{f}"))))
                .collect();
            body!("    return ({ct}){{ {} }};", fields.join(", "));
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            for (f, t) in &r.fields {
                let stmt = free_of(t, &format!("&v->{f}"));
                if !stmt.is_empty() {
                    body!("    {stmt}");
                }
            }
            body!("    (void)v;");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            let eqs: Vec<String> = r
                .fields
                .iter()
                .map(|(f, t)| eq_of(t, &format!("&a->{f}"), &format!("&b->{f}")))
                .collect();
            body!("    return {};", eqs.join(" && "));
            body!("}}");
            if set.hashed.contains(&n) {
                body!("static uint64_t {n}_hash(const {ct} *v) {{");
                body!("    uint64_t h = sudo_hash_u64(7);");
                for (f, t) in &r.fields {
                    body!("    h = sudo_hash_combine(h, {});", hash_of(t, &format!("&v->{f}")));
                }
                body!("    return h;");
                body!("}}");
            }
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    sudo_det_str(\"{{\\\"r\\\": \\\"{name}\\\", \\\"v\\\": [\");");
            for (i, (f, t)) in r.fields.iter().enumerate() {
                if i > 0 {
                    body!("    sudo_det_str(\", \");");
                }
                body!("    {}", canon_of(t, &format!("&v->{f}")));
            }
            body!("    sudo_det_str(\"]}}\");");
            body!("}}");
        }
        Ty::Enum(name) => {
            let en = m.enum_(name).expect("enum exists").clone();
            for v in &en.variants {
                let params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|(f, t)| format!("{} {f}", c_type(t)))
                    .collect();
                let args = if params.is_empty() { "void".to_string() } else { params.join(", ") };
                proto(&format!("{ct} {name}_{}({args})", v.name));
            }
            proto(&format!("{ct} {n}_copy(const {ct} *v)"));
            proto(&format!("void {n}_free({ct} *v)"));
            proto(&format!("bool {n}_eq(const {ct} *a, const {ct} *b)"));
            if set.hashed.contains(&n) {
                proto(&format!("uint64_t {n}_hash(const {ct} *v)"));
            }
            proto(&format!("void {n}_canon(const {ct} *v)"));

            for v in &en.variants {
                let params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|(f, t)| format!("{} {f}", c_type(t)))
                    .collect();
                let args = if params.is_empty() { "void".to_string() } else { params.join(", ") };
                body!("static {ct} {name}_{}({args}) {{", v.name);
                body!("    {ct} r; memset(&r, 0, sizeof r);");
                body!("    r.tag = {name}_{}_TAG;", v.name);
                for (f, t) in &v.fields {
                    if boxed_in_payload(t) {
                        let ft = c_type(t);
                        body!("    r.as.{0}.{f} = sudo_alloc(sizeof({ft}));", v.name);
                        body!("    *r.as.{0}.{f} = {f};", v.name);
                    } else {
                        body!("    r.as.{0}.{f} = {f};", v.name);
                    }
                }
                body!("    return r;");
                body!("}}");
            }
            body!("static {ct} {n}_copy(const {ct} *v) {{");
            body!("    switch (v->tag) {{");
            for v in &en.variants {
                body!("    case {name}_{}_TAG: {{", v.name);
                let args: Vec<String> = v
                    .fields
                    .iter()
                    .map(|(f, t)| {
                        let slot = if boxed_in_payload(t) {
                            format!("v->as.{}.{f}", v.name)
                        } else {
                            format!("&v->as.{}.{f}", v.name)
                        };
                        copy_of(t, &slot)
                    })
                    .collect();
                body!("        return {name}_{}({});", v.name, args.join(", "));
                body!("    }}");
            }
            body!("    }}");
            body!("    sudo_trap(SUDO_TRAP_INVALID_ARG, 0);");
            body!("}}");
            body!("static void {n}_free({ct} *v) {{");
            body!("    switch (v->tag) {{");
            for v in &en.variants {
                body!("    case {name}_{}_TAG: {{", v.name);
                for (f, t) in &v.fields {
                    if boxed_in_payload(t) {
                        let stmt = free_of(t, &format!("v->as.{}.{f}", v.name));
                        if !stmt.is_empty() {
                            body!("        {stmt}");
                        }
                        body!("        sudo_dealloc(v->as.{}.{f});", v.name);
                    } else {
                        let stmt = free_of(t, &format!("&v->as.{}.{f}", v.name));
                        if !stmt.is_empty() {
                            body!("        {stmt}");
                        }
                    }
                }
                body!("        break;");
                body!("    }}");
            }
            body!("    }}");
            body!("}}");
            body!("static bool {n}_eq(const {ct} *a, const {ct} *b) {{");
            body!("    if (a->tag != b->tag) return false;");
            body!("    switch (a->tag) {{");
            for v in &en.variants {
                body!("    case {name}_{}_TAG:", v.name);
                if v.fields.is_empty() {
                    body!("        return true;");
                } else {
                    let eqs: Vec<String> = v
                        .fields
                        .iter()
                        .map(|(f, t)| {
                            let (sa, sb) = if boxed_in_payload(t) {
                                (format!("a->as.{}.{f}", v.name), format!("b->as.{}.{f}", v.name))
                            } else {
                                (format!("&a->as.{}.{f}", v.name), format!("&b->as.{}.{f}", v.name))
                            };
                            eq_of(t, &sa, &sb)
                        })
                        .collect();
                    body!("        return {};", eqs.join(" && "));
                }
            }
            body!("    }}");
            body!("    return false;");
            body!("}}");
            body!("static void {n}_canon(const {ct} *v) {{");
            body!("    switch (v->tag) {{");
            for v in &en.variants {
                body!("    case {name}_{}_TAG:", v.name);
                if v.fields.is_empty() {
                    body!("        sudo_det_str(\"{{\\\"e\\\": \\\"{name}.{}\\\"}}\");", v.name);
                } else {
                    body!("        sudo_det_str(\"{{\\\"e\\\": \\\"{name}.{}\\\", \\\"v\\\": [\");", v.name);
                    for (i, (f, t)) in v.fields.iter().enumerate() {
                        if i > 0 {
                            body!("        sudo_det_str(\", \");");
                        }
                        let slot = if boxed_in_payload(t) {
                            format!("v->as.{}.{f}", v.name)
                        } else {
                            format!("&v->as.{}.{f}", v.name)
                        };
                        body!("        {}", canon_of(t, &slot));
                    }
                    body!("        sudo_det_str(\"]}}\");");
                }
                body!("        break;");
            }
            body!("    }}");
            body!("}}");
            if set.hashed.contains(&n) {
                body!("static uint64_t {n}_hash(const {ct} *v) {{");
                body!("    uint64_t h = sudo_hash_u64((uint64_t)v->tag);");
                body!("    switch (v->tag) {{");
                for v in &en.variants {
                    body!("    case {name}_{}_TAG:", v.name);
                    for (f, t) in &v.fields {
                        let slot = if boxed_in_payload(t) {
                            format!("v->as.{}.{f}", v.name)
                        } else {
                            format!("&v->as.{}.{f}", v.name)
                        };
                        body!("        h = sudo_hash_combine(h, {});", hash_of(t, &slot));
                    }
                    body!("        break;");
                }
                body!("    }}");
                body!("    return h;");
                body!("}}");
            }
        }
        _ => {}
    }
    let _ = writeln!(bodies);
}
