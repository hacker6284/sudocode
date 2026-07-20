//! Indentation-aware lexer. Produces NEWLINE/INDENT/DEDENT per spec §1:
//! 4-space indent unit, tabs in leading whitespace are errors, physical lines
//! continue inside unclosed brackets, comments run `//` to end of line.

/// A token kind. Reserved *type* names (`int`, `List`, `Some`, …) lex as
/// `Ident` — the parser gives them meaning; reserved *words* get variants.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    /// The magnitude 2^63, valid only directly after unary minus (the only
    /// way to write the minimum int literal).
    IntMin,
    Float(f64),
    /// Text literal, already desugared to Unicode scalar values (spec §3).
    Text(Vec<i64>),
    /// Char literal as its scalar value.
    Char(i64),
    Ident(String),

    And,
    Assert,
    BreakKw,
    Case,
    ContinueKw,
    Downto,
    Else,
    Enum,
    ExpectTrap,
    Export,
    False,
    For,
    Func,
    If,
    Import,
    In,
    Inout,
    Match,
    Mod,
    Not,
    Or,
    Record,
    Return,
    Skip,
    Test,
    To,
    True,
    While,

    Plus,
    Minus,
    Star,
    Slash,
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Arrow,
    Underscore,

    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// 1-based source line.
    pub line: u32,
    /// 1-based source column (in characters).
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

const INDENT_UNIT: u32 = 4;

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(src).run()
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    indents: Vec<u32>,
    bracket_depth: u32,
    out: Vec<Token>,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            indents: vec![0],
            bracket_depth: 0,
            out: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, LexError> {
        Err(LexError { line: self.line, col: self.col, msg: msg.into() })
    }

    fn emit(&mut self, tok: Tok, line: u32, col: u32) {
        self.out.push(Token { tok, line, col });
    }

    fn run(mut self) -> Result<Vec<Token>, LexError> {
        loop {
            if self.bracket_depth == 0 && !self.start_of_line()? {
                break; // EOF reached while skipping blank lines
            }
            if !self.lex_line_tokens()? {
                break; // EOF mid-line
            }
        }
        // Implied final newline if the last real token wasn't one.
        if !matches!(self.out.last().map(|t| &t.tok), None | Some(Tok::Newline)) {
            self.emit(Tok::Newline, self.line, self.col);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.emit(Tok::Dedent, self.line, 1);
        }
        self.emit(Tok::Eof, self.line, self.col);
        Ok(self.out)
    }

    /// Handle indentation at a physical line start. Skips blank/comment-only
    /// lines entirely. Returns false at EOF.
    fn start_of_line(&mut self) -> Result<bool, LexError> {
        loop {
            let mut width: u32 = 0;
            loop {
                match self.peek() {
                    Some(' ') => {
                        self.bump();
                        width += 1;
                    }
                    Some('\t') => return self.err("tab in indentation (use 4 spaces)"),
                    _ => break,
                }
            }
            match self.peek() {
                None => return Ok(false),
                Some('\n') => {
                    self.bump(); // blank line
                    continue;
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        self.bump();
                        if c == '\n' {
                            break;
                        }
                    }
                    continue; // comment-only line
                }
                Some(_) => {
                    self.apply_indent(width)?;
                    return Ok(true);
                }
            }
        }
    }

    fn apply_indent(&mut self, width: u32) -> Result<(), LexError> {
        let current = *self.indents.last().expect("indent stack never empty");
        if width == current {
            return Ok(());
        }
        if width > current {
            if width != current + INDENT_UNIT {
                return self.err(format!(
                    "indentation must step by exactly {INDENT_UNIT} spaces (found {width}, enclosing block at {current})"
                ));
            }
            self.indents.push(width);
            self.emit(Tok::Indent, self.line, 1);
            return Ok(());
        }
        while *self.indents.last().unwrap() > width {
            self.indents.pop();
            self.emit(Tok::Dedent, self.line, 1);
        }
        if *self.indents.last().unwrap() != width {
            return self.err(format!("dedent to {width} spaces matches no enclosing block"));
        }
        Ok(())
    }

    /// Lex tokens until a newline is emitted (or suppressed inside brackets
    /// and the line continues). Returns false on EOF.
    fn lex_line_tokens(&mut self) -> Result<bool, LexError> {
        loop {
            let (line, col) = (self.line, self.col);
            let c = match self.peek() {
                None => return Ok(false),
                Some(c) => c,
            };
            match c {
                ' ' => {
                    self.bump();
                }
                '\t' => return self.err("tab characters are not allowed"),
                '\n' => {
                    self.bump();
                    if self.bracket_depth == 0 {
                        self.emit(Tok::Newline, line, col);
                        return Ok(true);
                    }
                    // Inside brackets: the physical line continues.
                }
                '/' if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                'a'..='z' | 'A'..='Z' | '_' => self.lex_word(line, col),
                '0'..='9' => self.lex_number(line, col)?,
                '"' => self.lex_text(line, col)?,
                '\'' => self.lex_char(line, col)?,
                _ => {
                    if !self.lex_operator(line, col)? {
                        return self.err(format!("unexpected character {c:?}"));
                    }
                }
            }
        }
    }

    fn lex_word(&mut self, line: u32, col: u32) {
        let mut word = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                word.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let tok = match word.as_str() {
            "and" => Tok::And,
            "assert" => Tok::Assert,
            "break" => Tok::BreakKw,
            "case" => Tok::Case,
            "continue" => Tok::ContinueKw,
            "downto" => Tok::Downto,
            "else" => Tok::Else,
            "enum" => Tok::Enum,
            "expect_trap" => Tok::ExpectTrap,
            "export" => Tok::Export,
            "false" => Tok::False,
            "for" => Tok::For,
            "func" => Tok::Func,
            "if" => Tok::If,
            "import" => Tok::Import,
            "in" => Tok::In,
            "inout" => Tok::Inout,
            "match" => Tok::Match,
            "mod" => Tok::Mod,
            "not" => Tok::Not,
            "or" => Tok::Or,
            "record" => Tok::Record,
            "return" => Tok::Return,
            "skip" => Tok::Skip,
            "test" => Tok::Test,
            "to" => Tok::To,
            "true" => Tok::True,
            "while" => Tok::While,
            "_" => Tok::Underscore,
            _ => Tok::Ident(word),
        };
        self.emit(tok, line, col);
    }

    fn lex_number(&mut self, line: u32, col: u32) -> Result<(), LexError> {
        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                digits.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let is_float =
            self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit());
        if is_float {
            digits.push('.');
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    digits.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
            let value: f64 = digits.parse().expect("digits.digits parses as f64");
            self.emit(Tok::Float(value), line, col);
        } else {
            match digits.parse::<i64>() {
                Ok(value) => self.emit(Tok::Int(value), line, col),
                Err(_) if digits == "9223372036854775808" => {
                    self.emit(Tok::IntMin, line, col)
                }
                Err(_) => {
                    return Err(LexError {
                        line,
                        col,
                        msg: format!("integer literal {digits} is out of int range"),
                    })
                }
            }
        }
        Ok(())
    }

    /// Read one character or escape sequence inside a text/char literal.
    fn lex_scalar(&mut self, delim: char) -> Result<i64, LexError> {
        let c = match self.bump() {
            None => return self.err("unterminated literal"),
            Some('\n') => return self.err("unterminated literal"),
            Some(c) => c,
        };
        if c != '\\' {
            return Ok(c as i64);
        }
        let e = match self.bump() {
            None => return self.err("unterminated escape"),
            Some(e) => e,
        };
        let value = match e {
            'n' => '\n' as i64,
            't' => '\t' as i64,
            'r' => '\r' as i64,
            '\\' => '\\' as i64,
            '"' => '"' as i64,
            '\'' => '\'' as i64,
            'u' => {
                if self.bump() != Some('{') {
                    return self.err("expected '{' after \\u");
                }
                let mut hex = String::new();
                loop {
                    match self.bump() {
                        Some('}') => break,
                        Some(h) if h.is_ascii_hexdigit() => hex.push(h),
                        _ => return self.err("bad \\u{...} escape"),
                    }
                }
                let n = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c as i64);
                match n {
                    Some(n) => n,
                    None => return self.err("\\u{...} is not a Unicode scalar value"),
                }
            }
            other => return self.err(format!("unknown escape \\{other} in {delim} literal")),
        };
        Ok(value)
    }

    fn lex_text(&mut self, line: u32, col: u32) -> Result<(), LexError> {
        self.bump(); // opening quote
        let mut scalars = Vec::new();
        loop {
            match self.peek() {
                None | Some('\n') => return self.err("unterminated text literal"),
                Some('"') => {
                    self.bump();
                    break;
                }
                Some(_) => scalars.push(self.lex_scalar('"')?),
            }
        }
        self.emit(Tok::Text(scalars), line, col);
        Ok(())
    }

    fn lex_char(&mut self, line: u32, col: u32) -> Result<(), LexError> {
        self.bump(); // opening quote
        if self.peek() == Some('\'') {
            return self.err("empty char literal");
        }
        let value = self.lex_scalar('\'')?;
        if self.bump() != Some('\'') {
            return self.err("char literal must contain exactly one character");
        }
        self.emit(Tok::Char(value), line, col);
        Ok(())
    }

    /// Returns Ok(false) if the character starts no operator.
    fn lex_operator(&mut self, line: u32, col: u32) -> Result<bool, LexError> {
        let c = self.peek().expect("caller checked");
        let two = |a: char| -> bool { a == '=' };
        let tok = match c {
            '+' => Tok::Plus,
            '-' => {
                if self.peek2() == Some('>') {
                    self.bump();
                    Tok::Arrow
                } else {
                    Tok::Minus
                }
            }
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '=' => {
                if self.peek2() == Some('=') {
                    self.bump();
                    Tok::EqEq
                } else {
                    Tok::Assign
                }
            }
            '!' => {
                if self.peek2().is_some_and(two) {
                    self.bump();
                    Tok::NotEq
                } else {
                    return self.err("'!' is not an operator (use 'not')");
                }
            }
            '<' => {
                if self.peek2().is_some_and(two) {
                    self.bump();
                    Tok::Le
                } else {
                    Tok::Lt
                }
            }
            '>' => {
                if self.peek2().is_some_and(two) {
                    self.bump();
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            '(' => {
                self.bracket_depth += 1;
                Tok::LParen
            }
            ')' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                Tok::RParen
            }
            '[' => {
                self.bracket_depth += 1;
                Tok::LBracket
            }
            ']' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                Tok::RBracket
            }
            ',' => Tok::Comma,
            ':' => Tok::Colon,
            '.' => Tok::Dot,
            _ => return Ok(false),
        };
        self.bump();
        self.emit(tok, line, col);
        Ok(true)
    }
}
