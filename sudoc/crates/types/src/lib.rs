//! Type checking, local inference (spec §11), and lowering to the typed IR.

mod finalize;
mod func_check;
mod hoist;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use sudoc_ir::{IrConst, IrEnum, IrModule, IrRecord, IrVariant, Ty};
use sudoc_syntax::ast::{self, Module, TypeExpr};

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

pub(crate) fn error<T>(line: u32, col: u32, msg: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError { line, col, msg: msg.into() })
}

/// Module-level context shared by all function checks.
pub(crate) struct ModuleCtx {
    pub records: HashMap<String, Vec<(String, Ty)>>,
    pub enums: HashMap<String, Vec<IrVariant>>,
    /// variant name -> enums declaring it (ambiguity detection).
    pub variants: HashMap<String, Vec<String>>,
    pub consts: HashMap<String, Ty>,
    /// Folded constant values (constants are scalar; folding happens in the
    /// checker so overflow is a compile error, not backend-divergent).
    pub const_vals: HashMap<String, ConstVal>,
    pub funcs: HashMap<String, FuncSig>,
    /// Generic function templates, instantiated on demand (spec lockstep.md §7).
    pub generics: HashMap<String, ast::FuncDecl>,
    pub inst: Rc<RefCell<InstState>>,
    /// Imported modules, by name (spec §9).
    pub deps: HashMap<String, DepExports>,
}

/// A folded module-constant value.
#[derive(Clone, Copy, Debug)]
pub enum ConstVal {
    I(i64),
    F(f64),
    B(bool),
}

/// What one module exposes to its importers. `inst` is shared with the
/// defining module so cross-module generic instantiations land there.
#[derive(Clone)]
pub(crate) struct DepExports {
    pub funcs: HashMap<String, FuncSig>,
    pub consts: HashMap<String, Ty>,
    pub generics: HashMap<String, ast::FuncDecl>,
    pub inst: Rc<RefCell<InstState>>,
}

/// True if a signature only mentions types that exist in every module
/// (module-local records/enums cannot cross boundaries in v1).
pub(crate) fn sig_portable(sig: &FuncSig) -> bool {
    fn portable(t: &Ty) -> bool {
        match t {
            Ty::Record(_) | Ty::Enum(_) => false,
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => portable(e),
            Ty::Map(k, v) => portable(k) && portable(v),
            Ty::Result_(a, b) => portable(a) && portable(b),
            Ty::Tuple(ts) => ts.iter().all(portable),
            Ty::Func { params, ret } => {
                params.iter().all(portable)
                    && ret.as_ref().map(|r| portable(r)).unwrap_or(true)
            }
            _ => true,
        }
    }
    sig.params.iter().all(|(t, _)| portable(t))
        && sig.ret.as_ref().map(portable).unwrap_or(true)
}

/// Monomorphization bookkeeping. Instantiations are requested during body
/// checking (via RefCell) and processed by a worklist afterwards.
#[derive(Default)]
pub(crate) struct InstState {
    /// mangled name -> concrete signature (registered at request time so
    /// recursive and repeated calls resolve immediately).
    pub sigs: HashMap<String, FuncSig>,
    /// (template name, type args, mangled name), pending body check.
    pub queue: Vec<(String, Vec<Ty>, String)>,
    /// Instantiations per template — a runaway (polymorphic recursion) guard.
    pub counts: HashMap<String, u32>,
}

/// Deterministic, human-readable type mangling for instantiated function
/// names (`sort__i64`, `id__List_i64`) — part of the IR contract, shared by
/// every backend.
pub(crate) fn mangle_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "i64".into(),
        Ty::Float => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::List(e) => format!("List_{}", mangle_ty(e)),
        Ty::Set(e) => format!("Set_{}", mangle_ty(e)),
        Ty::Map(k, v) => format!("Map_{}_{}", mangle_ty(k), mangle_ty(v)),
        Ty::Option_(e) => format!("Opt_{}", mangle_ty(e)),
        Ty::Result_(t, e) => format!("Res_{}_{}", mangle_ty(t), mangle_ty(e)),
        Ty::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(mangle_ty).collect();
            format!("Tup{}_{}", ts.len(), parts.join("_"))
        }
        Ty::Func { params, ret } => {
            let parts: Vec<String> = params.iter().map(mangle_ty).collect();
            let r = ret.as_ref().map(|r| mangle_ty(r)).unwrap_or_else(|| "void".into());
            format!("Fn_{}_to_{r}", parts.join("_"))
        }
        Ty::Record(n) | Ty::Enum(n) => n.clone(),
        Ty::Infer(_) => unreachable!("Infer escaped resolution"),
    }
}

pub(crate) fn instantiation_name(template: &str, type_args: &[Ty]) -> String {
    let parts: Vec<String> = type_args.iter().map(mangle_ty).collect();
    format!("{template}__{}", parts.join("_"))
}

/// Concrete Ty back to surface syntax, for template substitution.
fn ty_to_type_expr(ty: &Ty) -> TypeExpr {
    match ty {
        Ty::Int => TypeExpr::Int,
        Ty::Float => TypeExpr::Float,
        Ty::Bool => TypeExpr::Bool,
        Ty::List(e) => TypeExpr::List(Box::new(ty_to_type_expr(e))),
        Ty::Set(e) => TypeExpr::Set(Box::new(ty_to_type_expr(e))),
        Ty::Map(k, v) => {
            TypeExpr::Map(Box::new(ty_to_type_expr(k)), Box::new(ty_to_type_expr(v)))
        }
        Ty::Option_(e) => TypeExpr::Option_(Box::new(ty_to_type_expr(e))),
        Ty::Result_(t, e) => {
            TypeExpr::Result_(Box::new(ty_to_type_expr(t)), Box::new(ty_to_type_expr(e)))
        }
        Ty::Tuple(ts) => TypeExpr::Tuple(ts.iter().map(ty_to_type_expr).collect()),
        Ty::Func { params, ret } => TypeExpr::Func {
            params: params.iter().map(ty_to_type_expr).collect(),
            ret: ret.as_ref().map(|r| Box::new(ty_to_type_expr(r))),
        },
        Ty::Record(n) | Ty::Enum(n) => {
            TypeExpr::Named { qualifier: None, name: n.clone() }
        }
        Ty::Infer(_) => unreachable!(),
    }
}

fn subst_type_expr(te: &TypeExpr, map: &HashMap<String, TypeExpr>) -> TypeExpr {
    match te {
        TypeExpr::Named { qualifier: None, name } if map.contains_key(name) => {
            map[name].clone()
        }
        TypeExpr::List(t) => TypeExpr::List(Box::new(subst_type_expr(t, map))),
        TypeExpr::Set(t) => TypeExpr::Set(Box::new(subst_type_expr(t, map))),
        TypeExpr::Map(k, v) => TypeExpr::Map(
            Box::new(subst_type_expr(k, map)),
            Box::new(subst_type_expr(v, map)),
        ),
        TypeExpr::Option_(t) => TypeExpr::Option_(Box::new(subst_type_expr(t, map))),
        TypeExpr::Result_(t, e) => TypeExpr::Result_(
            Box::new(subst_type_expr(t, map)),
            Box::new(subst_type_expr(e, map)),
        ),
        TypeExpr::Tuple(ts) => {
            TypeExpr::Tuple(ts.iter().map(|t| subst_type_expr(t, map)).collect())
        }
        TypeExpr::Func { params, ret } => TypeExpr::Func {
            params: params.iter().map(|t| subst_type_expr(t, map)).collect(),
            ret: ret.as_ref().map(|r| Box::new(subst_type_expr(r, map))),
        },
        other => other.clone(),
    }
}

fn subst_stmts(stmts: &mut [ast::Stmt], map: &HashMap<String, TypeExpr>) {
    for s in stmts {
        match s {
            ast::Stmt::TypedAssign { ty, .. } => *ty = subst_type_expr(ty, map),
            ast::Stmt::If { arms, else_block, .. } => {
                for (_, b) in arms {
                    subst_stmts(b, map);
                }
                if let Some(b) = else_block {
                    subst_stmts(b, map);
                }
            }
            ast::Stmt::While { body, .. }
            | ast::Stmt::ForRange { body, .. }
            | ast::Stmt::ForIn { body, .. } => subst_stmts(body, map),
            ast::Stmt::Match { arms, .. } => {
                for a in arms {
                    subst_stmts(&mut a.body, map);
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct FuncSig {
    pub params: Vec<(Ty, bool)>, // (type, inout)
    pub ret: Option<Ty>,
}

const RESERVED_TYPE_NAMES: &[&str] = &[
    "int", "float", "bool", "text", "List", "Map", "Set", "Option", "Result", "Some",
    "None", "Ok", "Err",
];

/// A checked multi-module program: dependencies first, entry module last.
#[derive(Debug)]
pub struct Program {
    pub modules: Vec<IrModule>,
}

/// Check a parsed, import-free module and lower it to typed IR.
pub fn check(module: &Module, module_name: &str) -> Result<IrModule, Vec<TypeError>> {
    (|| {
        if let Some(import) = module.imports.first() {
            return error(
                import.line,
                1,
                "this entry point has imports; compile it as a program (check_program / the sudoc CLI)",
            );
        }
        let mut pending = check_module(module, module_name, HashMap::new())?;
        while drain_worklist(&mut pending)? {}
        let flags = local_inout_flags(&pending);
        hoist_all(&mut pending, &flags);
        Ok(pending.ir)
    })()
    .map_err(|e| vec![e])
}

/// Load, check, and monomorphize a whole program from its entry file.
/// Imports resolve to sibling `.sudo` files.
pub fn check_program(entry: &Path) -> Result<Program, Vec<TypeError>> {
    check_program_inner(entry).map_err(|e| vec![e])
}

fn check_program_inner(entry: &Path) -> Result<Program, TypeError> {
    // Load and topologically order the module graph.
    let dir = entry.parent().map(Path::to_path_buf).unwrap_or_default();
    let entry_name = entry
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| TypeError { line: 0, col: 0, msg: "bad entry file name".into() })?
        .to_string();
    let mut order: Vec<String> = Vec::new();
    let mut asts: HashMap<String, Module> = HashMap::new();
    let mut visiting: Vec<String> = Vec::new();
    load_modules(&dir, &entry_name, &mut order, &mut asts, &mut visiting)?;

    // Check each module with its dependencies' exports in scope.
    let mut pendings: Vec<Pending> = Vec::new();
    for name in &order {
        let module = &asts[name];
        let mut deps = HashMap::new();
        for imp in &module.imports {
            let dep = pendings
                .iter()
                .find(|p| p.ir.name == imp.name)
                .expect("loader orders dependencies first");
            deps.insert(imp.name.clone(), exports_of(dep));
        }
        pendings.push(check_module(module, name, deps)?);
    }

    // Global monomorphization: importers enqueue into definers; loop to
    // quiescence across the whole program.
    loop {
        let mut worked = false;
        for p in &mut pendings {
            worked |= drain_worklist(p)?;
        }
        if !worked {
            break;
        }
    }

    // Hoist with a program-wide inout-flag table.
    let mut global_flags = hoist::InoutFlags::new();
    for p in &pendings {
        for (fname, sig) in &p.ctx.funcs {
            let flags: Vec<bool> = sig.params.iter().map(|(_, io)| *io).collect();
            global_flags.insert(fname.clone(), flags.clone());
            global_flags.insert(format!("{}.{fname}", p.ir.name), flags);
        }
    }
    for p in &mut pendings {
        hoist_all(p, &global_flags);
    }

    Ok(Program { modules: pendings.into_iter().map(|p| p.ir).collect() })
}

fn load_modules(
    dir: &Path,
    name: &str,
    order: &mut Vec<String>,
    asts: &mut HashMap<String, Module>,
    visiting: &mut Vec<String>,
) -> Result<(), TypeError> {
    if asts.contains_key(name) {
        return Ok(());
    }
    if visiting.iter().any(|v| v == name) {
        visiting.push(name.to_string());
        return error(
            0,
            0,
            format!("circular import: {}", visiting.join(" -> ")),
        );
    }
    let path: PathBuf = dir.join(format!("{name}.sudo"));
    let src = std::fs::read_to_string(&path).map_err(|_| TypeError {
        line: 0,
        col: 0,
        msg: format!("cannot find module '{name}' (looked for {})", path.display()),
    })?;
    let module = sudoc_syntax::parse_source(&src)
        .map_err(|e| TypeError { line: e.line, col: e.col, msg: format!("{name}.sudo: {}", e.msg) })?;
    visiting.push(name.to_string());
    for imp in &module.imports {
        load_modules(dir, &imp.name, order, asts, visiting)?;
    }
    visiting.pop();
    order.push(name.to_string());
    asts.insert(name.to_string(), module);
    Ok(())
}

fn exports_of(p: &Pending) -> DepExports {
    DepExports {
        funcs: p.ctx.funcs.clone(),
        consts: p.ctx.consts.clone(),
        generics: p.ctx.generics.clone(),
        inst: Rc::clone(&p.ctx.inst),
    }
}

pub(crate) struct Pending {
    pub ctx: ModuleCtx,
    pub type_names: HashMap<String, bool>,
    pub ir: IrModule,
}

fn local_inout_flags(p: &Pending) -> hoist::InoutFlags {
    p.ctx
        .funcs
        .iter()
        .map(|(name, sig)| (name.clone(), sig.params.iter().map(|(_, io)| *io).collect()))
        .collect()
}

fn hoist_all(p: &mut Pending, flags: &hoist::InoutFlags) {
    for f in &mut p.ir.funcs {
        f.body = hoist::hoist_body(std::mem::take(&mut f.body), flags);
    }
    for t in &mut p.ir.tests {
        t.body = hoist::hoist_body(std::mem::take(&mut t.body), flags);
    }
}

/// Process this module's pending instantiations. Returns true if any work
/// was done (importers may have enqueued more into other modules).
fn drain_worklist(p: &mut Pending) -> Result<bool, TypeError> {
    let mut worked = false;
    loop {
        let next = p.ctx.inst.borrow_mut().queue.pop();
        let Some((template_name, type_args, mangled)) = next else { break };
        worked = true;
        let template = p.ctx.generics[&template_name].clone();
        {
            let mut inst = p.ctx.inst.borrow_mut();
            let count = inst.counts.entry(template_name.clone()).or_insert(0);
            *count += 1;
            if *count > 32 {
                return error(
                    template.line,
                    1,
                    format!(
                        "'{template_name}' instantiated more than 32 times — recursive generic instantiation is not supported"
                    ),
                );
            }
        }
        let map: HashMap<String, TypeExpr> = template
            .generics
            .iter()
            .cloned()
            .zip(type_args.iter().map(ty_to_type_expr))
            .collect();
        let mut concrete = template.clone();
        concrete.name = mangled.clone();
        concrete.generics.clear();
        for prm in &mut concrete.params {
            prm.ty = subst_type_expr(&prm.ty, &map);
        }
        if let Some(r) = &mut concrete.ret {
            *r = subst_type_expr(r, &map);
        }
        subst_stmts(&mut concrete.body, &map);
        let sig = p.ctx.inst.borrow().sigs[&mangled].clone();
        p.ctx.funcs.insert(mangled, sig);
        p.ir.funcs.push(func_check::check_func(&concrete, &p.ctx, &p.type_names)?);
    }
    Ok(worked)
}

fn check_module(
    module: &Module,
    module_name: &str,
    deps: HashMap<String, DepExports>,
) -> Result<Pending, TypeError> {
    let mut ctx = ModuleCtx {
        records: HashMap::new(),
        enums: HashMap::new(),
        variants: HashMap::new(),
        consts: HashMap::new(),
        const_vals: HashMap::new(),
        funcs: HashMap::new(),
        generics: HashMap::new(),
        inst: Rc::new(RefCell::new(InstState::default())),
        deps,
    };

    // Pass 1: type names, so records/enums can reference each other.
    let mut type_names: HashMap<String, bool> = HashMap::new(); // name -> is_record
    for decl in &module.decls {
        let (name, line, is_record) = match decl {
            ast::Decl::Record(r) => (&r.name, r.line, true),
            ast::Decl::Enum(e) => (&e.name, e.line, false),
            _ => continue,
        };
        check_name(name, line)?;
        if RESERVED_TYPE_NAMES.contains(&name.as_str()) {
            return error(line, 1, format!("'{name}' is a reserved type name"));
        }
        if type_names.insert(name.clone(), is_record).is_some() {
            return error(line, 1, format!("type '{name}' is declared twice"));
        }
    }

    // Pass 2: resolve record fields and enum variants.
    let mut ir_records = Vec::new();
    let mut ir_enums = Vec::new();
    for decl in &module.decls {
        match decl {
            ast::Decl::Record(r) => {
                let mut fields = Vec::new();
                for (fname, fty) in &r.fields {
                    check_name(fname, r.line)?;
                    fields.push((fname.clone(), resolve_type(fty, &type_names, r.line)?));
                }
                ctx.records.insert(r.name.clone(), fields.clone());
                ir_records.push(IrRecord { name: r.name.clone(), fields });
            }
            ast::Decl::Enum(e) => {
                let mut variants = Vec::new();
                for v in &e.variants {
                    check_name(&v.name, e.line)?;
                    let mut fields = Vec::new();
                    for (fname, fty) in &v.fields {
                        fields.push((fname.clone(), resolve_type(fty, &type_names, e.line)?));
                    }
                    ctx.variants.entry(v.name.clone()).or_default().push(e.name.clone());
                    variants.push(IrVariant { name: v.name.clone(), fields });
                }
                ctx.enums.insert(e.name.clone(), variants.clone());
                ir_enums.push(IrEnum { name: e.name.clone(), variants });
            }
            _ => {}
        }
    }

    // Pass 3: function signatures.
    for decl in &module.decls {
        let f = match decl {
            ast::Decl::Func(f) => f,
            _ => continue,
        };
        check_name(&f.name, f.line)?;
        if !f.generics.is_empty() {
            if f.export {
                return error(f.line, 1, format!(
                    "exported function '{}' cannot be generic — host-facing signatures must be concrete",
                    f.name
                ));
            }
            if ctx.generics.insert(f.name.clone(), f.clone()).is_some() {
                return error(f.line, 1, format!("function '{}' is declared twice", f.name));
            }
            continue;
        }
        let mut params = Vec::new();
        for p in &f.params {
            check_name(&p.name, f.line)?;
            params.push((resolve_type(&p.ty, &type_names, f.line)?, p.inout));
        }
        let ret = match &f.ret {
            Some(t) => Some(resolve_type(t, &type_names, f.line)?),
            None => None,
        };
        if f.export {
            validate_export_boundary(f, &params, ret.as_ref(), f.line)?;
        }
        if ctx.funcs.insert(f.name.clone(), FuncSig { params, ret }).is_some() {
            return error(f.line, 1, format!("function '{}' is declared twice", f.name));
        }
    }

    // Pass 4: module constants (scalar constant expressions only in v1).
    let mut ir_consts = Vec::new();
    for decl in &module.decls {
        let c = match decl {
            ast::Decl::Const(c) => c,
            _ => continue,
        };
        check_name(&c.name, c.line)?;
        let (value, folded) = func_check::check_const_expr(&c.value, &ctx)?;
        if ctx.consts.insert(c.name.clone(), value.ty.clone()).is_some() {
            return error(c.line, 1, format!("constant '{}' is declared twice", c.name));
        }
        ctx.const_vals.insert(c.name.clone(), folded);
        ir_consts.push(IrConst { name: c.name.clone(), ty: value.ty.clone(), value });
    }

    // Pass 5: function and test bodies. Instantiation requests accumulate in
    // ctx.inst; worklists (local or program-wide) monomorphize them later.
    let mut ir_funcs = Vec::new();
    let mut ir_tests = Vec::new();
    let mut test_names = HashMap::new();
    for decl in &module.decls {
        match decl {
            ast::Decl::Func(f) => {
                if !f.generics.is_empty() {
                    continue; // template; instantiated on demand
                }
                ir_funcs.push(func_check::check_func(f, &ctx, &type_names)?);
            }
            ast::Decl::Test(t) => {
                if test_names.insert(t.name.clone(), ()).is_some() {
                    return error(t.line, 1, format!("test \"{}\" is declared twice", t.name));
                }
                ir_tests.push(func_check::check_test(t, &ctx)?);
            }
            _ => {}
        }
    }

    let ir = IrModule {
        name: module_name.to_string(),
        imports: module.imports.iter().map(|i| i.name.clone()).collect(),
        records: ir_records,
        enums: ir_enums,
        consts: ir_consts,
        funcs: ir_funcs,
        tests: ir_tests,
    };
    Ok(Pending { ctx, type_names, ir })
}

/// Convenience: parse + check.
pub fn check_source(src: &str, module_name: &str) -> Result<IrModule, Vec<TypeError>> {
    let module = sudoc_syntax::parse_source(src)
        .map_err(|e| vec![TypeError { line: e.line, col: e.col, msg: e.msg }])?;
    check(&module, module_name)
}

fn ret_has_nested_option(t: &Ty) -> bool {
    match t {
        Ty::Option_(inner) => matches!(**inner, Ty::Option_(_)) || ret_has_nested_option(inner),
        Ty::List(e) | Ty::Set(e) => ret_has_nested_option(e),
        Ty::Map(k, v) => ret_has_nested_option(k) || ret_has_nested_option(v),
        Ty::Result_(a, b) => ret_has_nested_option(a) || ret_has_nested_option(b),
        Ty::Tuple(ts) => ts.iter().any(ret_has_nested_option),
        _ => false,
    }
}

fn ret_has_func(t: &Ty) -> bool {
    match t {
        Ty::Func { .. } => true,
        Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => ret_has_func(e),
        Ty::Map(k, v) => ret_has_func(k) || ret_has_func(v),
        Ty::Result_(a, b) => ret_has_func(a) || ret_has_func(b),
        Ty::Tuple(ts) => ts.iter().any(ret_has_func),
        _ => false,
    }
}

/// Host-boundary rules for exports (lockstep.md §5): no ambiguous Option
/// collapse, no inout of types a host binding cannot be written back into,
/// no function-typed values across the boundary.
fn validate_export_boundary(
    f: &ast::FuncDecl,
    params: &[(Ty, bool)],
    ret: Option<&Ty>,
    line: u32,
) -> Result<(), TypeError> {
    if let Some(rt) = ret {
        if ret_has_nested_option(rt) {
            return error(line, 1, format!(
                "exported function '{}' returns Option<Option<...>>, which collapses ambiguously at the host boundary",
                f.name
            ));
        }
        if ret_has_func(rt) {
            return error(line, 1, format!(
                "exported function '{}' returns a function type, which cannot cross the host boundary",
                f.name
            ));
        }
    }
    fn contains_func(t: &Ty) -> bool {
        match t {
            Ty::Func { .. } => true,
            Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => contains_func(e),
            Ty::Map(k, v) => contains_func(k) || contains_func(v),
            Ty::Result_(a, b) => contains_func(a) || contains_func(b),
            Ty::Tuple(ts) => ts.iter().any(contains_func),
            _ => false,
        }
    }
    for ((ty, inout), p) in params.iter().zip(&f.params) {
        if contains_func(ty) {
            return error(line, 1, format!(
                "exported function '{}': parameter '{}' has a function type, which cannot cross the host boundary",
                f.name, p.name
            ));
        }
        if *inout {
            let ok = matches!(
                ty,
                Ty::List(_) | Ty::Map(..) | Ty::Set(_) | Ty::Record(_)
            ) && !matches!(&p.ty, TypeExpr::Text);
            if !ok {
                return error(line, 1, format!(
                    "exported function '{}': inout parameter '{}' must be a List, Map, Set, or record — hosts cannot write back into a {} binding",
                    f.name, p.name, ty
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn check_name(name: &str, line: u32) -> Result<(), TypeError> {
    if name.starts_with("_sudo_") {
        return error(line, 1, format!("identifiers starting with '_sudo_' are reserved: '{name}'"));
    }
    Ok(())
}

/// Resolve a surface type to a concrete `Ty`. The `text` alias erases here.
pub(crate) fn resolve_type(
    t: &TypeExpr,
    type_names: &HashMap<String, bool>,
    line: u32,
) -> Result<Ty, TypeError> {
    resolve_type_with(t, type_names, &HashMap::new(), line)
}

/// Like `resolve_type`, but names in `gmap` (generic parameters) resolve to
/// the given types (typically fresh inference variables).
pub(crate) fn resolve_type_with(
    t: &TypeExpr,
    type_names: &HashMap<String, bool>,
    gmap: &HashMap<String, Ty>,
    line: u32,
) -> Result<Ty, TypeError> {
    let ty = match t {
        TypeExpr::Int => Ty::Int,
        TypeExpr::Float => Ty::Float,
        TypeExpr::Bool => Ty::Bool,
        TypeExpr::Text => Ty::list(Ty::Int),
        TypeExpr::List(t) => Ty::List(Box::new(resolve_type_with(t, type_names, gmap, line)?)),
        TypeExpr::Set(t) => {
            let elem = resolve_type_with(t, type_names, gmap, line)?;
            require_hashable(&elem, "Set element", line)?;
            Ty::Set(Box::new(elem))
        }
        TypeExpr::Map(k, v) => {
            let k = resolve_type_with(k, type_names, gmap, line)?;
            require_hashable(&k, "Map key", line)?;
            Ty::Map(Box::new(k), Box::new(resolve_type_with(v, type_names, gmap, line)?))
        }
        TypeExpr::Option_(t) => Ty::Option_(Box::new(resolve_type_with(t, type_names, gmap, line)?)),
        TypeExpr::Result_(t, e) => Ty::Result_(
            Box::new(resolve_type_with(t, type_names, gmap, line)?),
            Box::new(resolve_type_with(e, type_names, gmap, line)?),
        ),
        TypeExpr::Tuple(ts) => Ty::Tuple(
            ts.iter()
                .map(|t| resolve_type_with(t, type_names, gmap, line))
                .collect::<Result<_, _>>()?,
        ),
        TypeExpr::Func { params, ret } => Ty::Func {
            params: params
                .iter()
                .map(|t| resolve_type_with(t, type_names, gmap, line))
                .collect::<Result<_, _>>()?,
            ret: match ret {
                Some(r) => Some(Box::new(resolve_type_with(r, type_names, gmap, line)?)),
                None => None,
            },
        },
        TypeExpr::Named { qualifier: Some(q), name } => {
            return error(line, 1, format!("unknown type '{q}.{name}' (imports arrive in M5)"));
        }
        TypeExpr::Named { qualifier: None, name } => {
            if let Some(t) = gmap.get(name) {
                t.clone()
            } else {
                match type_names.get(name) {
                    Some(true) => Ty::Record(name.clone()),
                    Some(false) => Ty::Enum(name.clone()),
                    None => return error(line, 1, format!("unknown type '{name}'")),
                }
            }
        }
    };
    Ok(ty)
}

/// Spec §2.2: int, bool, and tuples/records/enums/Lists of hashable types.
pub(crate) fn is_hashable(ty: &Ty, ctx: Option<&ModuleCtx>, seen: &mut Vec<String>) -> bool {
    match ty {
        Ty::Int | Ty::Bool => true,
        Ty::Float | Ty::Map(..) | Ty::Set(..) | Ty::Func { .. } | Ty::Infer(_) => false,
        Ty::List(t) | Ty::Option_(t) => is_hashable(t, ctx, seen),
        Ty::Result_(t, e) => is_hashable(t, ctx, seen) && is_hashable(e, ctx, seen),
        Ty::Tuple(ts) => ts.iter().all(|t| is_hashable(t, ctx, seen)),
        Ty::Record(name) => match ctx {
            None => true, // structure not known during signature resolution
            Some(ctx) => {
                if seen.contains(name) {
                    return true; // coinductive: assume ok on cycles
                }
                seen.push(name.clone());
                ctx.records.get(name).is_some_and(|fields| {
                    fields.iter().all(|(_, t)| is_hashable(t, Some(ctx), seen))
                })
            }
        },
        Ty::Enum(name) => match ctx {
            None => true,
            Some(ctx) => {
                if seen.contains(name) {
                    return true;
                }
                seen.push(name.clone());
                ctx.enums.get(name).is_some_and(|variants| {
                    variants
                        .iter()
                        .all(|v| v.fields.iter().all(|(_, t)| is_hashable(t, Some(ctx), seen)))
                })
            }
        },
    }
}

pub(crate) fn require_hashable(ty: &Ty, what: &str, line: u32) -> Result<(), TypeError> {
    if is_hashable(ty, None, &mut Vec::new()) {
        Ok(())
    } else {
        error(line, 1, format!("{what} type {ty} is not hashable (float, Map, Set, and func types cannot be keys)"))
    }
}
