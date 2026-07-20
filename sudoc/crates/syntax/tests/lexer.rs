use sudoc_syntax::{lex, LexError, Tok};

/// Lex and strip position info, keeping just the token kinds.
fn kinds(src: &str) -> Vec<Tok> {
    lex(src).expect("lex ok").into_iter().map(|t| t.tok).collect()
}

fn err(src: &str) -> LexError {
    lex(src).expect_err("expected lex error")
}

#[test]
fn simple_assignment() {
    assert_eq!(
        kinds("x = 1\n"),
        vec![
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::Int(1),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn final_newline_is_implied() {
    // Same tokens with or without trailing newline.
    assert_eq!(kinds("x = 1"), kinds("x = 1\n"));
}

#[test]
fn keywords_are_not_identifiers() {
    assert_eq!(
        kinds("if a and not b\n"),
        vec![
            Tok::If,
            Tok::Ident("a".into()),
            Tok::And,
            Tok::Not,
            Tok::Ident("b".into()),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn operators_and_punctuation() {
    assert_eq!(
        kinds("a == b != c <= d >= e -> f\n"),
        vec![
            Tok::Ident("a".into()),
            Tok::EqEq,
            Tok::Ident("b".into()),
            Tok::NotEq,
            Tok::Ident("c".into()),
            Tok::Le,
            Tok::Ident("d".into()),
            Tok::Ge,
            Tok::Ident("e".into()),
            Tok::Arrow,
            Tok::Ident("f".into()),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn mod_is_a_keyword_operator() {
    assert_eq!(
        kinds("a mod b\n"),
        vec![
            Tok::Ident("a".into()),
            Tok::Mod,
            Tok::Ident("b".into()),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn indentation_produces_indent_dedent() {
    let src = "while x\n    x = 1\ny = 2\n";
    assert_eq!(
        kinds(src),
        vec![
            Tok::While,
            Tok::Ident("x".into()),
            Tok::Newline,
            Tok::Indent,
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::Int(1),
            Tok::Newline,
            Tok::Dedent,
            Tok::Ident("y".into()),
            Tok::Assign,
            Tok::Int(2),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn eof_closes_all_open_blocks() {
    let src = "if a\n    if b\n        x = 1";
    let toks = kinds(src);
    let dedents = toks.iter().filter(|t| **t == Tok::Dedent).count();
    assert_eq!(dedents, 2);
    assert_eq!(toks.last(), Some(&Tok::Eof));
}

#[test]
fn dedent_two_levels_at_once() {
    let src = "if a\n    if b\n        x = 1\ny = 2\n";
    let toks = kinds(src);
    // After Int(1)+Newline there must be two consecutive Dedents before y.
    let pos = toks.iter().position(|t| *t == Tok::Ident("y".into())).unwrap();
    assert_eq!(&toks[pos - 2..pos], &[Tok::Dedent, Tok::Dedent]);
}

#[test]
fn blank_and_comment_lines_are_skipped() {
    let src = "x = 1\n\n// a comment\n    // indented comment, still skipped\ny = 2\n";
    assert_eq!(
        kinds(src),
        vec![
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::Int(1),
            Tok::Newline,
            Tok::Ident("y".into()),
            Tok::Assign,
            Tok::Int(2),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn trailing_comment_does_not_eat_newline() {
    assert_eq!(
        kinds("x = 1 // set x\n"),
        vec![
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::Int(1),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn brackets_suppress_newlines() {
    let src = "x = [1,\n    2,\n    3]\n";
    assert_eq!(
        kinds(src),
        vec![
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::LBracket,
            Tok::Int(1),
            Tok::Comma,
            Tok::Int(2),
            Tok::Comma,
            Tok::Int(3),
            Tok::RBracket,
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn float_and_int_literals() {
    assert_eq!(
        kinds("a = 1.5 + 2\n"),
        vec![
            Tok::Ident("a".into()),
            Tok::Assign,
            Tok::Float(1.5),
            Tok::Plus,
            Tok::Int(2),
            Tok::Newline,
            Tok::Eof
        ]
    );
}

#[test]
fn float_requires_digits_on_both_sides() {
    // "1." is an int followed by a dot (parser will reject); ".5" is a lone dot.
    assert_eq!(
        kinds("x = 1.\n")[2..4],
        [Tok::Int(1), Tok::Dot]
    );
}

#[test]
fn text_literal_desugars_to_scalars() {
    assert_eq!(
        kinds("s = \"abc\"\n")[2],
        Tok::Text(vec![97, 98, 99])
    );
}

#[test]
fn text_escapes() {
    assert_eq!(
        kinds("s = \"a\\n\\\"\\u{1F600}\"\n")[2],
        Tok::Text(vec![97, 10, 34, 0x1F600])
    );
}

#[test]
fn char_literal_is_scalar_value() {
    assert_eq!(kinds("c = 'a'\n")[2], Tok::Char(97));
    assert_eq!(kinds("c = '\\n'\n")[2], Tok::Char(10));
}

#[test]
fn underscore_alone_is_wildcard() {
    assert_eq!(kinds("_\n")[0], Tok::Underscore);
    // But an identifier may contain/start with underscore.
    assert_eq!(kinds("_x\n")[0], Tok::Ident("_x".into()));
}

#[test]
fn tab_in_indentation_is_an_error() {
    let e = err("if a\n\tx = 1\n");
    assert_eq!(e.line, 2);
    assert!(e.msg.contains("tab"), "msg was: {}", e.msg);
}

#[test]
fn indent_must_be_multiple_of_unit() {
    let e = err("if a\n  x = 1\n");
    assert_eq!(e.line, 2);
}

#[test]
fn dedent_must_match_enclosing_level() {
    // 8 -> 4 is fine, 8 -> 6 is not.
    let e = err("if a\n    if b\n        x = 1\n      y = 2\n");
    assert_eq!(e.line, 4);
}

#[test]
fn int_literal_overflow_is_an_error() {
    let e = err("x = 99999999999999999999\n");
    assert_eq!(e.line, 1);
    assert!(e.msg.contains("range"), "msg was: {}", e.msg);
    // Exactly 2^63 lexes as the special minimum-magnitude token, valid only
    // after unary minus (the parser enforces placement).
    assert_eq!(kinds("x = -9223372036854775808\n")[2..4], [Tok::Minus, Tok::IntMin]);
}

#[test]
fn unterminated_text_is_an_error() {
    assert!(lex("s = \"abc\n").is_err());
}

#[test]
fn non_ascii_outside_literals_is_an_error() {
    assert!(lex("π = 1\n").is_err());
    // ...but fine inside text literals:
    assert_eq!(kinds("s = \"π\"\n")[2], Tok::Text(vec![0x3C0]));
}

#[test]
fn positions_are_one_based() {
    let toks = lex("x = 1\n").unwrap();
    assert_eq!((toks[0].line, toks[0].col), (1, 1));
    assert_eq!((toks[1].line, toks[1].col), (1, 3));
    assert_eq!((toks[2].line, toks[2].col), (1, 5));
}
