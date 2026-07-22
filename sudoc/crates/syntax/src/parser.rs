//! Recursive-descent parser over the token stream (spec §10).

use crate::ast::*;
use crate::lexer::{Tok, Token};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

pub fn parse(tokens: Vec<Token>) -> Result<Module, ParseError> {
    Parser { toks: tokens, pos: 0 }.module()
}

/// Convenience: lex + parse.
pub fn parse_source(src: &str) -> Result<Module, ParseError> {
    let tokens = crate::lexer::lex(src)
        .map_err(|e| ParseError { line: e.line, col: e.col, msg: e.msg })?;
    parse(tokens)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, ParseError>;

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek2(&self) -> Option<&Tok> {
        self.toks.get(self.pos + 1).map(|t| &t.tok)
    }

    fn here(&self) -> (u32, u32) {
        let t = &self.toks[self.pos];
        (t.line, t.col)
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.at(tok) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> PResult<()> {
        if self.eat(tok) {
            Ok(())
        } else {
            self.err(format!("expected {what}, found {:?}", self.peek()))
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        let (line, col) = self.here();
        Err(ParseError { line, col, msg: msg.into() })
    }

    fn ident(&mut self, what: &str) -> PResult<String> {
        match self.peek() {
            Tok::Ident(name) => {
                let name = name.clone();
                self.bump();
                Ok(name)
            }
            _ => self.err(format!("expected {what}, found {:?}", self.peek())),
        }
    }

    // ---- declarations -----------------------------------------------------

    fn module(&mut self) -> PResult<Module> {
        let mut imports = Vec::new();
        while self.at(&Tok::Import) {
            let (line, _) = self.here();
            self.bump();
            let first = self.ident("module name")?;
            let (name, is_std) = if self.eat(&Tok::Dot) {
                let second = self.ident("module name")?;
                if first != "std" {
                    return self.err(format!(
                        "'{first}.{second}' is not a valid import — only the \
                         'std.' qualifier is supported (e.g. 'import std.{second}')"
                    ));
                }
                (second, true)
            } else {
                (first, false)
            };
            self.expect(&Tok::Newline, "end of line")?;
            imports.push(Import { name, is_std, line });
        }
        let mut decls = Vec::new();
        while !self.at(&Tok::Eof) {
            decls.push(self.decl()?);
        }
        Ok(Module { imports, decls })
    }

    fn decl(&mut self) -> PResult<Decl> {
        match self.peek() {
            Tok::Export | Tok::Func => Ok(Decl::Func(self.func_decl()?)),
            Tok::Record => Ok(Decl::Record(self.record_decl()?)),
            Tok::Enum => Ok(Decl::Enum(self.enum_decl()?)),
            Tok::Test => Ok(Decl::Test(self.test_decl()?)),
            Tok::Ident(_) if self.peek2() == Some(&Tok::Assign) => {
                let (line, _) = self.here();
                let name = self.ident("constant name")?;
                self.bump(); // =
                let value = self.expr()?;
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Decl::Const(ConstDecl { name, ty: None, value, line }))
            }
            Tok::Ident(_) if self.peek2() == Some(&Tok::Colon) => {
                // NAME: Type = expr  (optional annotation, like local TypedAssign)
                let (line, _) = self.here();
                let name = self.ident("constant name")?;
                self.bump(); // :
                let ty = self.type_expr()?;
                self.expect(&Tok::Assign, "'=' after annotated constant")?;
                let value = self.expr()?;
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Decl::Const(ConstDecl { name, ty: Some(ty), value, line }))
            }
            Tok::Import => self.err("imports must come before all declarations"),
            _ => self.err(format!(
                "expected a declaration (func, record, enum, test, or constant), found {:?}",
                self.peek()
            )),
        }
    }

    fn func_decl(&mut self) -> PResult<FuncDecl> {
        let (line, _) = self.here();
        let export = self.eat(&Tok::Export);
        self.expect(&Tok::Func, "'func'")?;
        let name = self.ident("function name")?;
        let mut generics = Vec::new();
        if self.eat(&Tok::Lt) {
            loop {
                generics.push(self.ident("type parameter")?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::Gt, "'>'")?;
        }
        self.expect(&Tok::LParen, "'('")?;
        let mut params = Vec::new();
        if !self.at(&Tok::RParen) {
            loop {
                let pname = self.ident("parameter name")?;
                self.expect(&Tok::Colon, "':'")?;
                let inout = self.eat(&Tok::Inout);
                let ty = self.type_expr()?;
                params.push(Param { inout, name: pname, ty });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.type_expr()?) } else { None };
        let body = self.block()?;
        Ok(FuncDecl { export, name, generics, params, ret, body, line })
    }

    fn record_decl(&mut self) -> PResult<RecordDecl> {
        let (line, _) = self.here();
        self.bump(); // record
        let name = self.ident("record name")?;
        self.expect(&Tok::Newline, "end of line")?;
        self.expect(&Tok::Indent, "an indented field list")?;
        let mut fields = Vec::new();
        while !self.at(&Tok::Dedent) {
            let fname = self.ident("field name")?;
            self.expect(&Tok::Colon, "':'")?;
            let ty = self.type_expr()?;
            self.expect(&Tok::Newline, "end of line")?;
            fields.push((fname, ty));
        }
        self.bump(); // dedent
        if fields.is_empty() {
            return self.err("record must have at least one field");
        }
        Ok(RecordDecl { name, fields, line })
    }

    fn enum_decl(&mut self) -> PResult<EnumDecl> {
        let (line, _) = self.here();
        self.bump(); // enum
        let name = self.ident("enum name")?;
        self.expect(&Tok::Newline, "end of line")?;
        self.expect(&Tok::Indent, "an indented variant list")?;
        let mut variants = Vec::new();
        while !self.at(&Tok::Dedent) {
            let vname = self.ident("variant name")?;
            let mut fields = Vec::new();
            if self.eat(&Tok::LParen) {
                loop {
                    let fname = self.ident("variant field name")?;
                    self.expect(&Tok::Colon, "':'")?;
                    let ty = self.type_expr()?;
                    fields.push((fname, ty));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen, "')'")?;
            }
            self.expect(&Tok::Newline, "end of line")?;
            variants.push(Variant { name: vname, fields });
        }
        self.bump(); // dedent
        if variants.is_empty() {
            return self.err("enum must have at least one variant");
        }
        Ok(EnumDecl { name, variants, line })
    }

    fn test_decl(&mut self) -> PResult<TestDecl> {
        let (line, _) = self.here();
        self.bump(); // test
        let name = match self.peek() {
            Tok::Text(scalars) => {
                let name: String = scalars
                    .iter()
                    .map(|&s| char::from_u32(s as u32).unwrap_or('\u{FFFD}'))
                    .collect();
                self.bump();
                name
            }
            _ => return self.err("expected a test name in quotes"),
        };
        let body = self.block()?;
        Ok(TestDecl { name, body, line })
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self) -> PResult<Block> {
        self.expect(&Tok::Newline, "end of line before an indented block")?;
        self.expect(&Tok::Indent, "an indented block")?;
        let mut stmts = Vec::new();
        while !self.at(&Tok::Dedent) {
            stmts.push(self.stmt()?);
        }
        self.bump(); // dedent
        Ok(stmts)
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let (line, _) = self.here();
        match self.peek() {
            Tok::If => self.if_stmt(),
            Tok::While => {
                self.bump();
                let cond = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body, line })
            }
            Tok::For => self.for_stmt(),
            Tok::Match => self.match_stmt(),
            Tok::Return => {
                self.bump();
                let value =
                    if self.at(&Tok::Newline) { None } else { Some(self.expr()?) };
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Stmt::Return { value, line })
            }
            Tok::Assert => {
                self.bump();
                let cond = self.expr()?;
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Stmt::Assert { cond, line })
            }
            Tok::Skip => {
                self.bump();
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Stmt::Skip { line })
            }
            Tok::BreakKw => {
                self.bump();
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Stmt::Break { line })
            }
            Tok::ContinueKw => {
                self.bump();
                self.expect(&Tok::Newline, "end of line")?;
                Ok(Stmt::Continue { line })
            }
            Tok::ExpectTrap => {
                self.bump();
                let kind = self.ident("a trap kind (e.g. OutOfBounds)")?;
                let body = self.block()?;
                Ok(Stmt::ExpectTrap { kind, body, line })
            }
            _ => self.simple_stmt(line),
        }
    }

    /// Assignment, typed assignment, or a call statement.
    fn simple_stmt(&mut self, line: u32) -> PResult<Stmt> {
        let first = self.expr()?;

        if self.at(&Tok::Colon) {
            // items: List<int> = []
            let name = match first.kind {
                ExprKind::Var(name) => name,
                _ => return self.err("type annotations only apply to plain variables"),
            };
            self.bump(); // :
            let ty = self.type_expr()?;
            self.expect(&Tok::Assign, "'=' after annotated variable")?;
            let value = self.expr()?;
            self.expect(&Tok::Newline, "end of line")?;
            return Ok(Stmt::TypedAssign { name, ty, value, line });
        }

        let mut targets = vec![first];
        while self.eat(&Tok::Comma) {
            targets.push(self.expr()?);
        }
        if self.eat(&Tok::Assign) {
            let mut values = vec![self.expr()?];
            while self.eat(&Tok::Comma) {
                values.push(self.expr()?);
            }
            self.expect(&Tok::Newline, "end of line")?;
            return Ok(Stmt::Assign { targets, values, line });
        }

        if targets.len() == 1 && matches!(targets[0].kind, ExprKind::Call { .. }) {
            let expr = targets.into_iter().next().unwrap();
            self.expect(&Tok::Newline, "end of line")?;
            return Ok(Stmt::Expr { expr, line });
        }
        Err(ParseError {
            line,
            col: 1,
            msg: "expected a statement (an expression alone must be a call)".into(),
        })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let (line, _) = self.here();
        self.bump(); // if
        let cond = self.expr()?;
        let body = self.block()?;
        let mut arms = vec![(cond, body)];
        let mut else_block = None;
        while self.at(&Tok::Else) {
            self.bump();
            if self.eat(&Tok::If) {
                let cond = self.expr()?;
                let body = self.block()?;
                arms.push((cond, body));
            } else {
                else_block = Some(self.block()?);
                break;
            }
        }
        Ok(Stmt::If { arms, else_block, line })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        let (line, _) = self.here();
        self.bump(); // for
        let first = self.ident("loop variable")?;
        match self.peek() {
            Tok::Assign => {
                self.bump();
                let from = self.expr()?;
                let down = match self.bump() {
                    Tok::To => false,
                    Tok::Downto => true,
                    _ => return self.err("expected 'to' or 'downto' in for range"),
                };
                let to = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::ForRange { var: first, from, to, down, body, line })
            }
            Tok::Comma => {
                self.bump();
                let second = self.ident("second loop variable")?;
                self.expect(&Tok::In, "'in'")?;
                let iter = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::ForIn { vars: vec![first, second], iter, body, line })
            }
            Tok::In => {
                self.bump();
                let iter = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::ForIn { vars: vec![first], iter, body, line })
            }
            _ => self.err("expected '=', 'in', or ', name in' after loop variable"),
        }
    }

    fn match_stmt(&mut self) -> PResult<Stmt> {
        let (line, _) = self.here();
        self.bump(); // match
        let scrutinee = self.expr()?;
        self.expect(&Tok::Newline, "end of line")?;
        self.expect(&Tok::Indent, "indented case arms")?;
        let mut arms = Vec::new();
        while !self.at(&Tok::Dedent) {
            let (arm_line, _) = self.here();
            self.expect(&Tok::Case, "'case'")?;
            let pattern = self.pattern()?;
            let body = self.block()?;
            arms.push(MatchArm { pattern, body, line: arm_line });
        }
        self.bump(); // dedent
        if arms.is_empty() {
            return self.err("match must have at least one case");
        }
        Ok(Stmt::Match { scrutinee, arms, line })
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                Ok(Pattern::Int(v))
            }
            Tok::Char(v) => {
                self.bump();
                Ok(Pattern::Int(v))
            }
            Tok::Minus => {
                self.bump();
                match self.bump() {
                    Tok::Int(v) => Ok(Pattern::Int(v.wrapping_neg())),
                    Tok::IntMin => Ok(Pattern::Int(i64::MIN)),
                    _ => self.err("expected an integer after '-' in pattern"),
                }
            }
            Tok::True => {
                self.bump();
                Ok(Pattern::Bool(true))
            }
            Tok::False => {
                self.bump();
                Ok(Pattern::Bool(false))
            }
            Tok::Underscore => {
                self.bump();
                Ok(Pattern::Wildcard)
            }
            Tok::Ident(first) => {
                self.bump();
                let (qualifier, name) = if self.eat(&Tok::Dot) {
                    (Some(first), self.ident("variant name")?)
                } else {
                    (None, first)
                };
                let mut binders = Vec::new();
                if self.eat(&Tok::LParen) {
                    loop {
                        binders.push(self.ident("binder")?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "')'")?;
                }
                Ok(Pattern::Variant { qualifier, name, binders })
            }
            _ => self.err(format!("expected a pattern, found {:?}", self.peek())),
        }
    }

    // ---- expressions ------------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        self.logic()
    }

    /// `and`/`or` chains — one operator kind per unparenthesized level
    /// (spec §4: mixing them requires parentheses).
    fn logic(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_level()?;
        let mut seen: Option<BinaryOp> = None;
        loop {
            let op = match self.peek() {
                Tok::And => BinaryOp::And,
                Tok::Or => BinaryOp::Or,
                _ => return Ok(lhs),
            };
            if let Some(prev) = seen {
                if prev != op {
                    return self.err(
                        "mixing 'and' and 'or' is ambiguous; parentheses are required — write 'a or (b and c)' or '(a or b) and c'",
                    );
                }
            }
            seen = Some(op);
            let (line, col) = (lhs.line, lhs.col);
            self.bump();
            let rhs = self.not_level()?;
            lhs = Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
                line,
                col,
            };
        }
    }

    /// `not` sits between and/or and the comparisons, but its operand must be
    /// postfix, another `not`, or parenthesized — `not a == b` is refused.
    fn not_level(&mut self) -> PResult<Expr> {
        if !self.at(&Tok::Not) {
            return self.comparison();
        }
        let (line, col) = self.here();
        self.bump();
        let operand = self.not_operand()?;
        if matches!(
            self.peek(),
            Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge
        ) {
            return self.err(
                "'not' beside a comparison is ambiguous; parentheses are required — write 'not (a == b)' or '(not a) == b'",
            );
        }
        Ok(Expr {
            kind: ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(operand) },
            line,
            col,
        })
    }

    fn not_operand(&mut self) -> PResult<Expr> {
        if self.at(&Tok::Not) {
            let (line, col) = self.here();
            self.bump();
            let operand = self.not_operand()?;
            return Ok(Expr {
                kind: ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(operand) },
                line,
                col,
            });
        }
        self.postfix()
    }

    /// A single, non-chaining comparison (spec §4: `a < b < c` is refused).
    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.additive()?;
        let op = match self.peek() {
            Tok::Lt => BinaryOp::Lt,
            Tok::Le => BinaryOp::Le,
            Tok::Gt => BinaryOp::Gt,
            Tok::Ge => BinaryOp::Ge,
            Tok::EqEq => BinaryOp::Eq,
            Tok::NotEq => BinaryOp::Ne,
            _ => return Ok(lhs),
        };
        let (line, col) = (lhs.line, lhs.col);
        self.bump();
        let rhs = self.additive()?;
        if matches!(
            self.peek(),
            Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge
        ) {
            return self.err(
                "comparison operators do not chain; parentheses are required — write '(a == b) == c'",
            );
        }
        Ok(Expr {
            kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
            line,
            col,
        })
    }

    fn binary_level(
        &mut self,
        next: fn(&mut Self) -> PResult<Expr>,
        ops: &[(Tok, BinaryOp)],
    ) -> PResult<Expr> {
        let mut lhs = next(self)?;
        'outer: loop {
            for (tok, op) in ops {
                if self.at(tok) {
                    let (line, col) = (lhs.line, lhs.col);
                    self.bump();
                    let rhs = next(self)?;
                    lhs = Expr {
                        kind: ExprKind::Binary {
                            op: *op,
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                        },
                        line,
                        col,
                    };
                    continue 'outer;
                }
            }
            return Ok(lhs);
        }
    }

    fn additive(&mut self) -> PResult<Expr> {
        self.binary_level(
            Self::multiplicative,
            &[(Tok::Plus, BinaryOp::Add), (Tok::Minus, BinaryOp::Sub)],
        )
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        self.binary_level(
            Self::unary,
            &[
                (Tok::Star, BinaryOp::Mul),
                (Tok::Slash, BinaryOp::Div),
                (Tok::Mod, BinaryOp::Mod),
            ],
        )
    }

    fn unary(&mut self) -> PResult<Expr> {
        let (line, col) = self.here();
        if self.eat(&Tok::Minus) {
            if self.eat(&Tok::IntMin) {
                return Ok(Expr { kind: ExprKind::Int(i64::MIN), line, col });
            }
            let operand = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Unary { op: UnaryOp::Neg, operand: Box::new(operand) },
                line,
                col,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.atom()?;
        loop {
            let (line, col) = (expr.line, expr.col);
            if self.eat(&Tok::LParen) {
                let mut args = Vec::new();
                if !self.at(&Tok::RParen) {
                    loop {
                        let name = match (self.peek(), self.peek2()) {
                            (Tok::Ident(n), Some(Tok::Assign)) => {
                                let n = n.clone();
                                self.bump();
                                self.bump();
                                Some(n)
                            }
                            _ => None,
                        };
                        args.push(CallArg { name, value: self.expr()? });
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RParen, "')'")?;
                expr = Expr {
                    kind: ExprKind::Call { callee: Box::new(expr), args },
                    line,
                    col,
                };
            } else if self.eat(&Tok::LBracket) {
                let index = self.expr()?;
                self.expect(&Tok::RBracket, "']'")?;
                expr = Expr {
                    kind: ExprKind::Index { recv: Box::new(expr), index: Box::new(index) },
                    line,
                    col,
                };
            } else if self.at(&Tok::Dot) {
                self.bump();
                let name = self.ident("field or method name")?;
                expr = Expr {
                    kind: ExprKind::Field { recv: Box::new(expr), name },
                    line,
                    col,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    fn atom(&mut self) -> PResult<Expr> {
        let (line, col) = self.here();
        let kind = match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Char(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Text(scalars) => {
                self.bump();
                ExprKind::Text(scalars)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Ident(name) => {
                self.bump();
                ExprKind::Var(name)
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(&Tok::RBracket) {
                    loop {
                        items.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RBracket, "']'")?;
                ExprKind::ListLit(items)
            }
            Tok::LParen => {
                self.bump();
                let first = self.expr()?;
                if self.eat(&Tok::Comma) {
                    let mut items = vec![first];
                    loop {
                        items.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "')'")?;
                    ExprKind::TupleLit(items)
                } else {
                    self.expect(&Tok::RParen, "')'")?;
                    return Ok(first);
                }
            }
            Tok::IntMin => {
                return self.err(
                    "9223372036854775808 exceeds the int range; only -9223372036854775808 is valid",
                )
            }
            other => return self.err(format!("expected an expression, found {other:?}")),
        };
        Ok(Expr { kind, line, col })
    }

    // ---- types ------------------------------------------------------------

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        match self.peek().clone() {
            Tok::Func => {
                self.bump();
                self.expect(&Tok::LParen, "'('")?;
                let mut params = Vec::new();
                if !self.at(&Tok::RParen) {
                    loop {
                        params.push(self.type_expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RParen, "')'")?;
                let ret = if self.eat(&Tok::Arrow) {
                    Some(Box::new(self.type_expr()?))
                } else {
                    None
                };
                Ok(TypeExpr::Func { params, ret })
            }
            Tok::LParen => {
                self.bump();
                let mut items = vec![self.type_expr()?];
                self.expect(&Tok::Comma, "',' (tuple types have at least two elements)")?;
                loop {
                    items.push(self.type_expr()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen, "')'")?;
                Ok(TypeExpr::Tuple(items))
            }
            Tok::Ident(name) => {
                self.bump();
                match name.as_str() {
                    "int" => Ok(TypeExpr::Int),
                    "float" => Ok(TypeExpr::Float),
                    "bool" => Ok(TypeExpr::Bool),
                    "text" => Ok(TypeExpr::Text),
                    "List" => Ok(TypeExpr::List(Box::new(self.one_type_arg()?))),
                    "Set" => Ok(TypeExpr::Set(Box::new(self.one_type_arg()?))),
                    "Option" => Ok(TypeExpr::Option_(Box::new(self.one_type_arg()?))),
                    "Map" => {
                        let (k, v) = self.two_type_args()?;
                        Ok(TypeExpr::Map(Box::new(k), Box::new(v)))
                    }
                    "Result" => {
                        let (t, e) = self.two_type_args()?;
                        Ok(TypeExpr::Result_(Box::new(t), Box::new(e)))
                    }
                    _ => {
                        if self.at(&Tok::Dot) {
                            self.bump();
                            let inner = self.ident("type name")?;
                            Ok(TypeExpr::Named { qualifier: Some(name), name: inner })
                        } else {
                            Ok(TypeExpr::Named { qualifier: None, name })
                        }
                    }
                }
            }
            other => self.err(format!("expected a type, found {other:?}")),
        }
    }

    fn one_type_arg(&mut self) -> PResult<TypeExpr> {
        self.expect(&Tok::Lt, "'<'")?;
        let ty = self.type_expr()?;
        self.expect(&Tok::Gt, "'>'")?;
        Ok(ty)
    }

    fn two_type_args(&mut self) -> PResult<(TypeExpr, TypeExpr)> {
        self.expect(&Tok::Lt, "'<'")?;
        let a = self.type_expr()?;
        self.expect(&Tok::Comma, "','")?;
        let b = self.type_expr()?;
        self.expect(&Tok::Gt, "'>'")?;
        Ok((a, b))
    }
}
