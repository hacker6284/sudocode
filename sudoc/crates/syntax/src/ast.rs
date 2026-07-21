//! AST for sudo, mirroring the grammar in spec/language.md §10.
//! Char literals desugar to ints at parse time; text literals keep their
//! scalar vectors so backends can render them readably.

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub imports: Vec<Import>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub name: String,
    /// True for `import std.name` (spec §9) — resolves only against the
    /// embedded stdlib, never the filesystem, regardless of what files
    /// exist on disk. `name` holds just `name` (the part after `std.`),
    /// never `"std"` itself.
    pub is_std: bool,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Func(FuncDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    Test(TestDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDecl {
    pub export: bool,
    pub name: String,
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub inout: bool,
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<(String, TypeExpr)>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<Variant>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<(String, TypeExpr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub name: String,
    pub value: Expr,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestDecl {
    pub name: String,
    pub body: Block,
    pub line: u32,
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `a, b = x, y` — targets are validated as assignable during checking.
    Assign { targets: Vec<Expr>, values: Vec<Expr>, line: u32 },
    /// `items: List<int> = []`
    TypedAssign { name: String, ty: TypeExpr, value: Expr, line: u32 },
    /// Expression statement — must be a call (checked later).
    Expr { expr: Expr, line: u32 },
    If { arms: Vec<(Expr, Block)>, else_block: Option<Block>, line: u32 },
    While { cond: Expr, body: Block, line: u32 },
    ForRange { var: String, from: Expr, to: Expr, down: bool, body: Block, line: u32 },
    /// `for x in c` / `for k, v in m`
    ForIn { vars: Vec<String>, iter: Expr, body: Block, line: u32 },
    Match { scrutinee: Expr, arms: Vec<MatchArm>, line: u32 },
    Return { value: Option<Expr>, line: u32 },
    Assert { cond: Expr, line: u32 },
    Skip { line: u32 },
    Break { line: u32 },
    Continue { line: u32 },
    /// Test-only, final statement: the block must trap `kind`.
    ExpectTrap { kind: String, body: Block, line: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Block,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Int(i64),
    Bool(bool),
    Wildcard,
    /// `Node(v, l, r)` or `Tree.Node(v, l, r)`; nullary variants have no parens.
    Variant { qualifier: Option<String>, name: String, binders: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Text literal (scalar values) — semantically a `List<int>`.
    Text(Vec<i64>),
    Var(String),
    ListLit(Vec<Expr>),
    TupleLit(Vec<Expr>),
    /// `f(x)`, `m.get(k)`, `sorting.quicksort(a)` — callee is Var or Field;
    /// resolution (function vs method vs constructor) happens in checking.
    /// Args may be named (`Point(x = 1, y = 2)`) — records only, checked later.
    Call { callee: Box<Expr>, args: Vec<CallArg> },
    /// `a.length`, `p.x`, `module.name` — resolution happens in checking.
    Field { recv: Box<Expr>, name: String },
    Index { recv: Box<Expr>, index: Box<Expr> },
    Unary { op: UnaryOp, operand: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    Int,
    Float,
    Bool,
    /// `text` — alias of `List<int>` carrying boundary intent.
    Text,
    List(Box<TypeExpr>),
    Set(Box<TypeExpr>),
    Map(Box<TypeExpr>, Box<TypeExpr>),
    Option_(Box<TypeExpr>),
    Result_(Box<TypeExpr>, Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Func { params: Vec<TypeExpr>, ret: Option<Box<TypeExpr>> },
    /// User record/enum or generic parameter; optionally module-qualified.
    Named { qualifier: Option<String>, name: String },
}
