//! Lexer, parser, and AST for the sudo language (spec/language.md).

pub mod ast;
pub mod lexer;
pub mod parser;

pub use lexer::{lex, LexError, Tok, Token};
pub use parser::{parse, parse_source, ParseError};
