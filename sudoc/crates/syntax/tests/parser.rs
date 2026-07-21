use sudoc_syntax::ast::*;
use sudoc_syntax::parse_source;

fn module(src: &str) -> Module {
    parse_source(src).expect("parse ok")
}

/// Parse a source whose single decl is a function; return it.
fn func(src: &str) -> FuncDecl {
    let m = module(src);
    match m.decls.into_iter().next().expect("one decl") {
        Decl::Func(f) => f,
        other => panic!("expected func, got {other:?}"),
    }
}

/// Shorthand: first statement of a single function's body.
fn first_stmt(src: &str) -> Stmt {
    func(src).body.into_iter().next().expect("one stmt")
}

#[test]
fn function_signature() {
    let f = func("export func gcd(a: int, b: int) -> int\n    return a\n");
    assert!(f.export);
    assert_eq!(f.name, "gcd");
    assert!(f.generics.is_empty());
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name, "a");
    assert_eq!(f.params[0].ty, TypeExpr::Int);
    assert!(!f.params[0].inout);
    assert_eq!(f.ret, Some(TypeExpr::Int));
}

#[test]
fn inout_and_no_return_type() {
    let f = func("func sort_it(items: inout List<int>)\n    skip\n");
    assert!(!f.export);
    assert!(f.params[0].inout);
    assert_eq!(f.params[0].ty, TypeExpr::List(Box::new(TypeExpr::Int)));
    assert_eq!(f.ret, None);
}

#[test]
fn generic_function() {
    let f = func("func sort<T>(items: inout List<T>, less: func(T, T) -> bool)\n    skip\n");
    assert_eq!(f.generics, vec!["T".to_string()]);
    assert_eq!(
        f.params[1].ty,
        TypeExpr::Func {
            params: vec![
                TypeExpr::Named { qualifier: None, name: "T".into() },
                TypeExpr::Named { qualifier: None, name: "T".into() }
            ],
            ret: Some(Box::new(TypeExpr::Bool)),
        }
    );
}

#[test]
fn imports_then_decls() {
    let m = module("import sorting\n\nx = 1\n");
    assert_eq!(m.imports.len(), 1);
    assert_eq!(m.imports[0].name, "sorting");
    assert!(matches!(&m.decls[0], Decl::Const(c) if c.name == "x"));
}

#[test]
fn import_std_parses_is_std_true() {
    let m = module("import std.regex\n");
    assert_eq!(m.imports.len(), 1);
    assert_eq!(m.imports[0].name, "regex");
    assert!(m.imports[0].is_std);
}

#[test]
fn plain_import_parses_is_std_false() {
    let m = module("import regex\n");
    assert_eq!(m.imports.len(), 1);
    assert_eq!(m.imports[0].name, "regex");
    assert!(!m.imports[0].is_std);
}

#[test]
fn non_std_dotted_import_is_parse_error() {
    let err = parse_source("import foo.bar\n").expect_err("should not parse");
    assert!(
        err.msg.contains("foo.bar") || err.msg.contains("std."),
        "{}",
        err.msg
    );
}

#[test]
fn record_decl() {
    let m = module("record Point\n    x: int\n    y: float\n");
    match &m.decls[0] {
        Decl::Record(r) => {
            assert_eq!(r.name, "Point");
            assert_eq!(r.fields[0], ("x".into(), TypeExpr::Int));
            assert_eq!(r.fields[1], ("y".into(), TypeExpr::Float));
        }
        other => panic!("expected record, got {other:?}"),
    }
}

#[test]
fn enum_decl() {
    let m = module("enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\n");
    match &m.decls[0] {
        Decl::Enum(e) => {
            assert_eq!(e.name, "Tree");
            assert_eq!(e.variants[0].name, "Leaf");
            assert!(e.variants[0].fields.is_empty());
            assert_eq!(e.variants[1].fields.len(), 3);
            assert_eq!(
                e.variants[1].fields[1],
                ("left".into(), TypeExpr::Named { qualifier: None, name: "Tree".into() })
            );
        }
        other => panic!("expected enum, got {other:?}"),
    }
}

#[test]
fn test_decl() {
    let m = module("test \"sorts things\"\n    assert true\n");
    match &m.decls[0] {
        Decl::Test(t) => {
            assert_eq!(t.name, "sorts things");
            assert!(matches!(t.body[0], Stmt::Assert { .. }));
        }
        other => panic!("expected test, got {other:?}"),
    }
}

#[test]
fn if_else_if_else() {
    let s = first_stmt(
        "func f(x: int) -> int\n    if x < 0\n        return 0\n    else if x == 0\n        return 1\n    else\n        return 2\n",
    );
    match s {
        Stmt::If { arms, else_block, .. } => {
            assert_eq!(arms.len(), 2);
            assert!(else_block.is_some());
        }
        other => panic!("expected if, got {other:?}"),
    }
}

#[test]
fn while_loop() {
    let s = first_stmt("func f()\n    while true\n        skip\n");
    assert!(matches!(s, Stmt::While { .. }));
}

#[test]
fn for_to_and_downto() {
    let up = first_stmt("func f(n: int)\n    for i = 0 to n - 1\n        skip\n");
    match up {
        Stmt::ForRange { var, down, .. } => {
            assert_eq!(var, "i");
            assert!(!down);
        }
        other => panic!("{other:?}"),
    }
    let down = first_stmt("func f(n: int)\n    for i = n - 1 downto 0\n        skip\n");
    assert!(matches!(down, Stmt::ForRange { down: true, .. }));
}

#[test]
fn for_in_one_and_two_vars() {
    let one = first_stmt("func f(items: List<int>)\n    for x in items\n        skip\n");
    match one {
        Stmt::ForIn { vars, .. } => assert_eq!(vars, vec!["x".to_string()]),
        other => panic!("{other:?}"),
    }
    let two = first_stmt("func f(m: Map<int, int>)\n    for k, v in m\n        skip\n");
    match two {
        Stmt::ForIn { vars, .. } => assert_eq!(vars, vec!["k".to_string(), "v".to_string()]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn match_with_patterns() {
    let s = first_stmt(
        "func f(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Tree.Node(v, l, r)\n            return v\n        case _\n            skip\n    return 1\n",
    );
    match s {
        Stmt::Match { arms, .. } => {
            assert_eq!(
                arms[0].pattern,
                Pattern::Variant { qualifier: None, name: "Leaf".into(), binders: vec![] }
            );
            assert_eq!(
                arms[1].pattern,
                Pattern::Variant {
                    qualifier: Some("Tree".into()),
                    name: "Node".into(),
                    binders: vec!["v".into(), "l".into(), "r".into()]
                }
            );
            assert_eq!(arms[2].pattern, Pattern::Wildcard);
            assert!(matches!(arms[2].body[0], Stmt::Skip { .. }));
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn literal_patterns() {
    let s = first_stmt(
        "func f(x: int) -> int\n    match x\n        case 0\n            return 0\n        case -1\n            return 1\n        case 'a'\n            return 2\n        case _\n            return 3\n",
    );
    match s {
        Stmt::Match { arms, .. } => {
            assert_eq!(arms[0].pattern, Pattern::Int(0));
            assert_eq!(arms[1].pattern, Pattern::Int(-1));
            assert_eq!(arms[2].pattern, Pattern::Int(97));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn assignments() {
    let s = first_stmt("func f()\n    a, b = b, a\n");
    match s {
        Stmt::Assign { targets, values, .. } => {
            assert_eq!(targets.len(), 2);
            assert_eq!(values.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    let s = first_stmt("func f()\n    items: List<int> = []\n");
    match s {
        Stmt::TypedAssign { name, ty, .. } => {
            assert_eq!(name, "items");
            assert_eq!(ty, TypeExpr::List(Box::new(TypeExpr::Int)));
        }
        other => panic!("{other:?}"),
    }
    let s = first_stmt("func f(a: List<int>)\n    a[0] = 1\n");
    match s {
        Stmt::Assign { targets, .. } => {
            assert!(matches!(targets[0].kind, ExprKind::Index { .. }));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn call_statement_and_method_chain() {
    let s = first_stmt("func f(q: List<int>)\n    q.append(3)\n");
    match s {
        Stmt::Expr { expr, .. } => match expr.kind {
            ExprKind::Call { callee, args } => {
                assert_eq!(args.len(), 1);
                assert!(matches!(callee.kind, ExprKind::Field { .. }));
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn precedence() {
    // 1 + 2 * 3  parses as  1 + (2 * 3)
    let s = first_stmt("func f()\n    x = 1 + 2 * 3\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    match value.kind {
        ExprKind::Binary { op: BinaryOp::Add, rhs, .. } => {
            assert!(matches!(rhs.kind, ExprKind::Binary { op: BinaryOp::Mul, .. }));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ambiguous_forms_require_parentheses() {
    // Spec §4: where languages disagree, sudo refuses to guess.
    let e = parse_source("func f(a: bool, b: bool)\n    x = not a == b\n").unwrap_err();
    assert!(e.msg.to_lowercase().contains("parenthes"), "{}", e.msg);
    let e = parse_source("func f(a: bool, b: bool, c: bool)\n    x = a or b and c\n").unwrap_err();
    assert!(e.msg.to_lowercase().contains("parenthes"), "{}", e.msg);
    let e = parse_source("func f(a: int, b: int, c: int)\n    x = a < b < c\n").unwrap_err();
    assert!(e.msg.to_lowercase().contains("parenthes") || e.msg.to_lowercase().contains("chain"), "{}", e.msg);
    let e = parse_source("func f(a: bool, b: bool, c: bool)\n    x = a == b == c\n").unwrap_err();
    assert!(e.msg.to_lowercase().contains("parenthes") || e.msg.to_lowercase().contains("chain"), "{}", e.msg);
}

#[test]
fn parenthesized_forms_and_clear_forms_parse() {
    // The disambiguated versions all work.
    first_stmt("func f(a: bool, b: bool)\n    x = not (a == b)\n");
    first_stmt("func f(a: bool, b: bool)\n    x = (not a) == b\n");
    first_stmt("func f(a: bool, b: bool, c: bool)\n    x = a or (b and c)\n");
    first_stmt("func f(a: bool, b: bool, c: bool)\n    x = a and b and c\n");
    first_stmt("func f(a: int, b: int, c: int)\n    x = (a < b) == (b < c)\n");
    // `not` over postfix chains stays natural.
    let s = first_stmt("func f(m: Map<int, int>, k: int)\n    x = not m.has(k)\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    assert!(matches!(value.kind, ExprKind::Unary { op: UnaryOp::Not, .. }));
    // `not not x` is legal (operand is another not).
    first_stmt("func f(a: bool)\n    x = not not a\n");
    // and/or with comparison operands are fine — comparisons bind tighter.
    let s = first_stmt("func f(i: int, n: int)\n    x = i >= 0 and i < n\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    assert!(matches!(value.kind, ExprKind::Binary { op: BinaryOp::And, .. }));
}

#[test]
fn postfix_chain() {
    // adj.get(node).get_or([])
    let s = first_stmt("func f(adj: Map<int, List<int>>, node: int)\n    x = adj.get(node).get_or([])\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    match value.kind {
        ExprKind::Call { callee, .. } => match callee.kind {
            ExprKind::Field { recv, name } => {
                assert_eq!(name, "get_or");
                assert!(matches!(recv.kind, ExprKind::Call { .. }));
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn tuple_vs_grouping() {
    let s = first_stmt("func f()\n    x = (1, 2)\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    assert!(matches!(value.kind, ExprKind::TupleLit(ref xs) if xs.len() == 2));

    let s = first_stmt("func f()\n    x = (1 + 2) * 3\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    assert!(matches!(value.kind, ExprKind::Binary { op: BinaryOp::Mul, .. }));
}

#[test]
fn literals() {
    let s = first_stmt("func f()\n    x = [1, 'a', \"hi\"]\n");
    let value = match s {
        Stmt::Assign { values, .. } => values.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    match value.kind {
        ExprKind::ListLit(items) => {
            assert!(matches!(items[0].kind, ExprKind::Int(1)));
            assert!(matches!(items[1].kind, ExprKind::Int(97))); // char desugars
            assert!(matches!(items[2].kind, ExprKind::Text(ref v) if *v == vec![104, 105]));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn nested_and_qualified_types() {
    let f = func("func f(a: Map<int, List<Option<int>>>, b: geo.Point, c: (int, float))\n    skip\n");
    assert_eq!(
        f.params[0].ty,
        TypeExpr::Map(
            Box::new(TypeExpr::Int),
            Box::new(TypeExpr::List(Box::new(TypeExpr::Option_(Box::new(TypeExpr::Int)))))
        )
    );
    assert_eq!(
        f.params[1].ty,
        TypeExpr::Named { qualifier: Some("geo".into()), name: "Point".into() }
    );
    assert_eq!(f.params[2].ty, TypeExpr::Tuple(vec![TypeExpr::Int, TypeExpr::Float]));
}

#[test]
fn return_with_and_without_value() {
    let with = first_stmt("func f() -> int\n    return 1 + 2\n");
    assert!(matches!(with, Stmt::Return { value: Some(_), .. }));
    let without = first_stmt("func f()\n    return\n");
    assert!(matches!(without, Stmt::Return { value: None, .. }));
}

#[test]
fn parse_errors_have_positions() {
    // Statement that is neither assignment nor call.
    let e = parse_source("func f()\n    1 + 2\n").unwrap_err();
    assert_eq!(e.line, 2);
    // Unexpected token at top level.
    assert!(parse_source("return 1\n").is_err());
    // Import after a decl.
    assert!(parse_source("x = 1\nimport foo\n").is_err());
}

#[test]
fn whole_example_files_parse() {
    // The committed examples are the living spec — they must all parse.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../examples");
    let mut checked = 0;
    for entry in walk(dir.as_ref()) {
        let src = std::fs::read_to_string(&entry).unwrap();
        if let Err(e) = parse_source(&src) {
            panic!("{} failed to parse: {}", entry.display(), e);
        }
        checked += 1;
    }
    assert!(checked >= 9, "expected at least 9 example files, found {checked}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("examples dir exists") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "sudo") {
            out.push(path);
        }
    }
    out
}
