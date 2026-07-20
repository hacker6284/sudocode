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
pub mod wire;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// i64 leaves on the wire are decimal strings so f64-based hosts keep full
/// precision beyond 2^53 (spec/protocol.md §3).
mod i64_str {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Finite floats are plain JSON numbers; non-finite values are the reserved
/// strings `"nan"` / `"inf"` / `"-inf"` so wire-trip never flakes (protocol §3).
mod f64_wire {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_nan() {
            s.serialize_str("nan")
        } else if *v == f64::INFINITY {
            s.serialize_str("inf")
        } else if *v == f64::NEG_INFINITY {
            s.serialize_str("-inf")
        } else {
            v.serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        match Value::deserialize(d)? {
            Value::Number(n) => n
                .as_f64()
                .ok_or_else(|| serde::de::Error::custom("float out of f64 range")),
            Value::String(s) => match s.as_str() {
                "nan" => Ok(f64::NAN),
                "inf" => Ok(f64::INFINITY),
                "-inf" => Ok(f64::NEG_INFINITY),
                other => Err(serde::de::Error::custom(format!(
                    "unknown float string {other:?}; expected \"nan\", \"inf\", or \"-inf\""
                ))),
            },
            _ => Err(serde::de::Error::custom(
                "expected a JSON number or \"nan\"/\"inf\"/\"-inf\"",
            )),
        }
    }
}

// Private mirror of finished-IR `Ty` for serde + schemars. Omits `Infer` so
// that variant cannot appear on the wire or in the generated schema; nested
// boxes stay as `TyWire` so generation does not re-enter `Ty`'s hand-written
// impls. Doc-comment deliberately empty so schemars does not surface this
// internal type's prose in `ir-schema.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Ty")]
enum TyWire {
    Int,
    Float,
    Bool,
    List(Box<TyWire>),
    Set(Box<TyWire>),
    Map(Box<TyWire>, Box<TyWire>),
    Option_(Box<TyWire>),
    Result_(Box<TyWire>, Box<TyWire>),
    Tuple(Vec<TyWire>),
    Func {
        params: Vec<TyWire>,
        ret: Option<Box<TyWire>>,
    },
    Record(String),
    Enum(String),
}

impl From<&Ty> for TyWire {
    fn from(ty: &Ty) -> Self {
        match ty {
            Ty::Int => TyWire::Int,
            Ty::Float => TyWire::Float,
            Ty::Bool => TyWire::Bool,
            Ty::List(t) => TyWire::List(Box::new(TyWire::from(t.as_ref()))),
            Ty::Set(t) => TyWire::Set(Box::new(TyWire::from(t.as_ref()))),
            Ty::Map(k, v) => TyWire::Map(
                Box::new(TyWire::from(k.as_ref())),
                Box::new(TyWire::from(v.as_ref())),
            ),
            Ty::Option_(t) => TyWire::Option_(Box::new(TyWire::from(t.as_ref()))),
            Ty::Result_(t, e) => TyWire::Result_(
                Box::new(TyWire::from(t.as_ref())),
                Box::new(TyWire::from(e.as_ref())),
            ),
            Ty::Tuple(ts) => TyWire::Tuple(ts.iter().map(TyWire::from).collect()),
            Ty::Func { params, ret } => TyWire::Func {
                params: params.iter().map(TyWire::from).collect(),
                ret: ret.as_ref().map(|r| Box::new(TyWire::from(r.as_ref()))),
            },
            Ty::Record(n) => TyWire::Record(n.clone()),
            Ty::Enum(n) => TyWire::Enum(n.clone()),
            Ty::Infer(_) => unreachable!("Ty::Infer is rejected before TyWire conversion"),
        }
    }
}

impl From<TyWire> for Ty {
    fn from(w: TyWire) -> Self {
        match w {
            TyWire::Int => Ty::Int,
            TyWire::Float => Ty::Float,
            TyWire::Bool => Ty::Bool,
            TyWire::List(t) => Ty::List(Box::new((*t).into())),
            TyWire::Set(t) => Ty::Set(Box::new((*t).into())),
            TyWire::Map(k, v) => Ty::Map(Box::new((*k).into()), Box::new((*v).into())),
            TyWire::Option_(t) => Ty::Option_(Box::new((*t).into())),
            TyWire::Result_(t, e) => {
                Ty::Result_(Box::new((*t).into()), Box::new((*e).into()))
            }
            TyWire::Tuple(ts) => Ty::Tuple(ts.into_iter().map(Into::into).collect()),
            TyWire::Func { params, ret } => Ty::Func {
                params: params.into_iter().map(Into::into).collect(),
                ret: ret.map(|r| Box::new((*r).into())),
            },
            TyWire::Record(n) => Ty::Record(n),
            TyWire::Enum(n) => Ty::Enum(n),
        }
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

impl Serialize for Ty {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Mirror enum excludes Infer so a leaked inference variable is a
        // hard serialize error rather than a silent wire encoding.
        match self {
            Ty::Infer(_) => Err(serde::ser::Error::custom(
                "Ty::Infer cannot be serialized — internal inference variable leaked into finished IR",
            )),
            other => TyWire::from(other).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Ty {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(TyWire::deserialize(deserializer)?.into())
    }
}

impl JsonSchema for Ty {
    fn schema_name() -> String {
        "Ty".to_owned()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        // Schema is the wire shape: no Infer branch.
        <TyWire as JsonSchema>::json_schema(gen)
    }
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

/// Closed boundary type for export signatures: the resolved type shape with
/// `text` preserved (unlike [`Ty`], which erases it to `List<int>`). Qualifiers
/// on named types are dropped — same collapse as [`Ty::Record`] / [`Ty::Enum`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BoundaryTy {
    Int,
    Float,
    Bool,
    Text,
    List(Box<BoundaryTy>),
    Set(Box<BoundaryTy>),
    Map(Box<BoundaryTy>, Box<BoundaryTy>),
    Option_(Box<BoundaryTy>),
    Result_(Box<BoundaryTy>, Box<BoundaryTy>),
    Tuple(Vec<BoundaryTy>),
    Func {
        params: Vec<BoundaryTy>,
        ret: Option<Box<BoundaryTy>>,
    },
    /// User record/enum by bare declaration name (no module qualifier).
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrRecord {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrEnum {
    pub name: String,
    pub variants: Vec<IrVariant>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrVariant {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrConst {
    pub name: String,
    pub ty: Ty,
    pub value: IrExpr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrFunc {
    pub name: String,
    pub export: bool,
    pub params: Vec<IrParam>,
    pub ret: Option<Ty>,
    /// Declared boundary return type for exports — this is where `text` survives.
    pub ret_boundary: Option<BoundaryTy>,
    pub body: Vec<IrStmt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrParam {
    pub name: String,
    pub inout: bool,
    pub ty: Ty,
    /// Declared boundary type for exports (surface shape with `text` intact).
    pub boundary: BoundaryTy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrTest {
    pub name: String,
    pub body: Vec<IrStmt>,
}

/// An assignable location. The checker guarantees the base of the chain is a
/// mutable local variable (or inout parameter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Place {
    Var(String),
    Index {
        base: Box<Place>,
        base_ty: Ty,
        index: Box<IrExpr>,
    },
    Field {
        base: Box<Place>,
        base_ty: Ty,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IrStmt {
    /// `declares` is true when this assignment introduces the variable
    /// (backends with declarations, like C, need the distinction).
    Assign {
        target: Place,
        value: IrExpr,
        declares: bool,
    },
    /// Destructure a tuple value into variables. `declares` aligns per-var.
    TupleAssign {
        targets: Vec<String>,
        declares: Vec<bool>,
        value: IrExpr,
    },
    Expr(IrExpr),
    If {
        arms: Vec<(IrExpr, Vec<IrStmt>)>,
        else_block: Option<Vec<IrStmt>>,
    },
    While {
        cond: IrExpr,
        body: Vec<IrStmt>,
    },
    ForRange {
        var: String,
        from: IrExpr,
        to: IrExpr,
        down: bool,
        body: Vec<IrStmt>,
    },
    /// Iterate a List (in order), Set, or Map (unspecified order — two vars).
    ForIn {
        vars: Vec<String>,
        iter: IrExpr,
        body: Vec<IrStmt>,
    },
    Match {
        scrutinee: IrExpr,
        arms: Vec<IrMatchArm>,
    },
    Return(Option<IrExpr>),
    /// `line` feeds trap diagnostics and harness reports.
    Assert {
        cond: IrExpr,
        line: u32,
    },
    Skip,
    /// Exits the innermost loop (surface `break`, and the lowering of
    /// `while` conditions containing inout-passing calls).
    Break,
    /// Skips to the innermost loop's next iteration (surface `continue`).
    Continue,
    /// Test-only, final statement: the block must trap `kind` (spec §5.4).
    ExpectTrap {
        kind: String,
        body: Vec<IrStmt>,
        line: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrMatchArm {
    pub pattern: IrPattern,
    pub body: Vec<IrStmt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IrPattern {
    Int(#[serde(with = "i64_str")] #[schemars(with = "String")] i64),
    Bool(bool),
    Wildcard,
    /// Enum name + variant name + binder names (full arity, in field order).
    Variant {
        enum_name: String,
        variant: String,
        binders: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrExpr {
    pub ty: Ty,
    pub kind: IrExprKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IrExprKind {
    Int(#[serde(with = "i64_str")] #[schemars(with = "String")] i64),
    Float(#[serde(with = "f64_wire")] #[schemars(with = "f64")] f64),
    Bool(bool),
    /// Text literal — type is `List<int>`; kept intact for readable output.
    /// Wire form is a plain JSON array of numbers (Unicode scalars), not strings.
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
    CallValue {
        callee: Box<IrExpr>,
        args: Vec<IrExpr>,
    },
    /// Record construction; args in field declaration order.
    NewRecord { name: String, args: Vec<IrExpr> },
    /// Enum variant construction; args in field declaration order.
    NewVariant {
        enum_name: String,
        variant: String,
        args: Vec<IrExpr>,
    },
    /// Non-mutating builtin (free function or method — receiver is `args[0]`).
    Builtin { builtin: Builtin, args: Vec<IrExpr> },
    /// Mutating builtin method; receiver is a place.
    MutBuiltin {
        builtin: Builtin,
        recv: Place,
        recv_ty: Ty,
        args: Vec<IrExpr>,
    },
    /// Record field read (`.length`/`.size` are Builtin, not Field).
    GetField { recv: Box<IrExpr>, name: String },
    /// List or Map indexing — traps OutOfBounds / KeyMissing.
    Index {
        recv: Box<IrExpr>,
        index: Box<IrExpr>,
    },
    Unary { op: UnaryOp, operand: Box<IrExpr> },
    Binary {
        op: BinaryOp,
        lhs: Box<IrExpr>,
        rhs: Box<IrExpr>,
    },
}

pub use sudoc_syntax::ast::{BinaryOp, UnaryOp};

/// Built-in functions and methods, fully resolved. Receiver-taking builtins
/// list the receiver as their first argument (or `recv` when mutating).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
