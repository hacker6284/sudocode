//! Typed, resolved IR for sudo — the contract between frontend and backends.
//!
//! By the time a module reaches this form: every expression carries its type,
//! every name is resolved (local, function, builtin, variant, record), the
//! `text` alias is erased to `List<int>` (kept only on export signatures for
//! boundary mapping), parallel assignments are lowered to temporaries, and
//! generics do not exist (rejected until M5's monomorphizer lands).
//! Backends are pretty-printers plus a small runtime — nothing here should
//! require them to make a semantic decision.

pub mod pretty;

pub use sudoc_syntax::ast::TypeExpr;

/// Canonical test-function naming, shared by every backend and the lockstep
/// harness so outcomes align by name across targets.
pub mod names {
    pub fn sanitize(name: &str) -> String {
        let mut out = String::new();
        let mut prev_us = false;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
                prev_us = false;
            } else if !prev_us && !out.is_empty() {
                out.push('_');
                prev_us = true;
            }
        }
        while out.ends_with('_') {
            out.pop();
        }
        if out.is_empty() {
            out.push('t');
        }
        out
    }

    /// Deduplicated `test_*` function names for a module's tests, in order.
    pub fn test_fn_names(tests: &[super::IrTest]) -> Vec<String> {
        let mut used = std::collections::HashSet::new();
        let mut out = Vec::new();
        for t in tests {
            let mut name = format!("test_{}", sanitize(&t.name));
            let mut n = 2;
            while !used.insert(name.clone()) {
                name = format!("test_{}_{n}", sanitize(&t.name));
                n += 1;
            }
            out.push(name);
        }
        out
    }
}

/// A fully resolved sudo type. `text` never appears — it erases to
/// `List(Int)`; boundary intent lives in `IrParam::boundary` / `IrFunc::ret_boundary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    List(Box<Ty>),
    Set(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Option_(Box<Ty>),
    Result_(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Func { params: Vec<Ty>, ret: Option<Box<Ty>> },
    /// User-defined record, by declaration name.
    Record(String),
    /// User-defined enum, by declaration name.
    Enum(String),
    /// Checker-internal inference variable. Never present in a finished
    /// `IrModule` — the checker's finalize pass replaces or rejects these.
    Infer(u32),
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Int => write!(f, "int"),
            Ty::Float => write!(f, "float"),
            Ty::Bool => write!(f, "bool"),
            Ty::List(t) => write!(f, "List<{t}>"),
            Ty::Set(t) => write!(f, "Set<{t}>"),
            Ty::Map(k, v) => write!(f, "Map<{k}, {v}>"),
            Ty::Option_(t) => write!(f, "Option<{t}>"),
            Ty::Result_(t, e) => write!(f, "Result<{t}, {e}>"),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Ty::Func { params, ret } => {
                write!(f, "func(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")?;
                if let Some(r) = ret {
                    write!(f, " -> {r}")?;
                }
                Ok(())
            }
            Ty::Record(n) | Ty::Enum(n) => write!(f, "{n}"),
            Ty::Infer(i) => write!(f, "?{i}"),
        }
    }
}

impl Ty {
    pub fn list(elem: Ty) -> Ty {
        Ty::List(Box::new(elem))
    }
    /// True for types stored inline with trivial copies in every backend.
    pub fn is_scalar(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Bool)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrModule {
    pub name: String,
    /// Module names this module imports (dependency order not implied).
    pub imports: Vec<String>,
    pub records: Vec<IrRecord>,
    pub enums: Vec<IrEnum>,
    pub consts: Vec<IrConst>,
    pub funcs: Vec<IrFunc>,
    pub tests: Vec<IrTest>,
}

impl IrModule {
    pub fn record(&self, name: &str) -> Option<&IrRecord> {
        self.records.iter().find(|r| r.name == name)
    }
    pub fn enum_(&self, name: &str) -> Option<&IrEnum> {
        self.enums.iter().find(|e| e.name == name)
    }
    pub fn func(&self, name: &str) -> Option<&IrFunc> {
        self.funcs.iter().find(|f| f.name == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrRecord {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrEnum {
    pub name: String,
    pub variants: Vec<IrVariant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrVariant {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrConst {
    pub name: String,
    pub ty: Ty,
    pub value: IrExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrFunc {
    pub name: String,
    pub export: bool,
    pub params: Vec<IrParam>,
    pub ret: Option<Ty>,
    /// Declared (surface) return type, for boundary mapping of exports —
    /// this is where `text` survives.
    pub ret_boundary: Option<TypeExpr>,
    pub body: Vec<IrStmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrParam {
    pub name: String,
    pub inout: bool,
    pub ty: Ty,
    /// Declared (surface) type, for boundary mapping of exports.
    pub boundary: TypeExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrTest {
    pub name: String,
    pub body: Vec<IrStmt>,
}

/// An assignable location. The checker guarantees the base of the chain is a
/// mutable local variable (or inout parameter).
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Var(String),
    Index { base: Box<Place>, base_ty: Ty, index: Box<IrExpr> },
    Field { base: Box<Place>, base_ty: Ty, name: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrStmt {
    /// `declares` is true when this assignment introduces the variable
    /// (backends with declarations, like C, need the distinction).
    Assign { target: Place, value: IrExpr, declares: bool },
    /// Destructure a tuple value into variables. `declares` aligns per-var.
    TupleAssign { targets: Vec<String>, declares: Vec<bool>, value: IrExpr },
    Expr(IrExpr),
    If { arms: Vec<(IrExpr, Vec<IrStmt>)>, else_block: Option<Vec<IrStmt>> },
    While { cond: IrExpr, body: Vec<IrStmt> },
    ForRange { var: String, from: IrExpr, to: IrExpr, down: bool, body: Vec<IrStmt> },
    /// Iterate a List (in order), Set, or Map (unspecified order — two vars).
    ForIn { vars: Vec<String>, iter: IrExpr, body: Vec<IrStmt> },
    Match { scrutinee: IrExpr, arms: Vec<IrMatchArm> },
    Return(Option<IrExpr>),
    /// `line` feeds trap diagnostics and harness reports.
    Assert { cond: IrExpr, line: u32 },
    Skip,
    /// Exits the innermost loop (surface `break`, and the lowering of
    /// `while` conditions containing inout-passing calls).
    Break,
    /// Skips to the innermost loop's next iteration (surface `continue`).
    Continue,
    /// Test-only, final statement: the block must trap `kind` (spec §5.4).
    ExpectTrap { kind: String, body: Vec<IrStmt>, line: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrMatchArm {
    pub pattern: IrPattern,
    pub body: Vec<IrStmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrPattern {
    Int(i64),
    Bool(bool),
    Wildcard,
    /// Enum name + variant name + binder names (full arity, in field order).
    Variant { enum_name: String, variant: String, binders: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrExpr {
    pub ty: Ty,
    pub kind: IrExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Text literal — type is `List<int>`; kept intact for readable output.
    Text(Vec<i64>),
    /// Local variable or parameter.
    Local(String),
    /// Module constant.
    Const(String),
    /// Reference to a top-level function (function-typed value).
    FuncRef(String),
    List(Vec<IrExpr>),
    Tuple(Vec<IrExpr>),
    /// Call to a user-defined function in this module.
    CallFunc { name: String, args: Vec<IrExpr> },
    /// Call through a function-typed local/parameter.
    CallValue { callee: Box<IrExpr>, args: Vec<IrExpr> },
    /// Record construction; args in field declaration order.
    NewRecord { name: String, args: Vec<IrExpr> },
    /// Enum variant construction; args in field declaration order.
    NewVariant { enum_name: String, variant: String, args: Vec<IrExpr> },
    /// Non-mutating builtin (free function or method — receiver is `args[0]`).
    Builtin { builtin: Builtin, args: Vec<IrExpr> },
    /// Mutating builtin method; receiver is a place.
    MutBuiltin { builtin: Builtin, recv: Place, recv_ty: Ty, args: Vec<IrExpr> },
    /// Record field read (`.length`/`.size` are Builtin, not Field).
    GetField { recv: Box<IrExpr>, name: String },
    /// List or Map indexing — traps OutOfBounds / KeyMissing.
    Index { recv: Box<IrExpr>, index: Box<IrExpr> },
    Unary { op: UnaryOp, operand: Box<IrExpr> },
    Binary { op: BinaryOp, lhs: Box<IrExpr>, rhs: Box<IrExpr> },
}

pub use sudoc_syntax::ast::{BinaryOp, UnaryOp};

/// Built-in functions and methods, fully resolved. Receiver-taking builtins
/// list the receiver as their first argument (or `recv` when mutating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    // free functions
    AbsInt,
    AbsFloat,
    MinInt,
    MaxInt,
    MinFloat,
    MaxFloat,
    FloatOfInt,
    IntOfFloat,
    Floor,
    Ceil,
    Round,
    Sqrt,
    Filled,
    NewMap,
    NewSet,
    // List (and text)
    ListLength,
    ListAppend,
    ListPop,
    ListInsert,
    ListRemoveAt,
    ListSwap,
    ListSort,
    // Map
    MapSize,
    MapGet,
    MapHas,
    MapDelete,
    MapKeys,
    MapValues,
    // Set
    SetSize,
    SetAdd,
    SetHas,
    SetRemove,
    SetItems,
    // Option / Result
    OptIsSome,
    OptIsNone,
    OptUnwrap,
    OptGetOr,
    ResIsOk,
    ResIsErr,
    ResUnwrap,
    ResGetOr,
}

impl Builtin {
    /// Does this builtin mutate its receiver?
    pub fn mutates(self) -> bool {
        matches!(
            self,
            Builtin::ListAppend
                | Builtin::ListPop
                | Builtin::ListInsert
                | Builtin::ListRemoveAt
                | Builtin::ListSwap
                | Builtin::ListSort
                | Builtin::MapDelete
                | Builtin::SetAdd
                | Builtin::SetRemove
        )
    }
}
