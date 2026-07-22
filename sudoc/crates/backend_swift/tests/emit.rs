use sudoc_backend_swift::emit;

fn swift(src: &str) -> String {
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    emit(&ir, true)
}

#[test]
fn gcd_reads_like_swift() {
    let out = swift(
        "export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n",
    );
    assert!(out.contains("func gcd(_ a: Int64, _ b: Int64)"), "{out}");
    // Params shadowed as var so reassignment works (Swift params are let).
    assert!(out.contains("var a: Int64 = a"), "{out}");
    assert!(out.contains("while b != Int64(0)"), "{out}");
    assert!(out.contains("floorMod"), "{out}");
    assert!(out.contains("chkAbs"), "{out}");
}

#[test]
fn int_arithmetic_is_checked() {
    let out = swift("func f(a: int, b: int) -> int\n    return a * b + 1\n");
    assert!(out.contains("chkMul"), "{out}");
    assert!(out.contains("chkAdd"), "{out}");
    assert!(out.contains("Int64(1)"), "{out}");
}

#[test]
fn float_arithmetic_is_plain() {
    let out = swift("func f(a: float, b: float) -> float\n    return a * b + 1.0\n");
    assert!(!out.contains("chkMul"), "{out}");
    assert!(!out.contains("chkAdd"), "{out}");
    assert!(out.contains("a * b"), "{out}");
}

#[test]
fn inout_uses_native_inout() {
    let out = swift(
        "func bump(x: inout int)\n    x = x + 1\nfunc f() -> int\n    n = 0\n    bump(n)\n    return n\n",
    );
    assert!(out.contains("inout Int64"), "{out}");
    assert!(out.contains("bump(&n)"), "{out}");
}

#[test]
fn composite_params_need_no_dup() {
    let out = swift("func f(items: List<int>) -> int\n    return items.length\n");
    assert!(!out.contains("dup"), "{out}");
    assert!(out.contains("items.count"), "{out}");
}

#[test]
fn equality_is_native() {
    let out = swift("func f(a: List<int>, b: List<int>) -> bool\n    return a == b\n");
    assert!(out.contains("a == b"), "{out}");
    assert!(!out.contains("sudoEq"), "{out}");
}

#[test]
fn enums_become_swift_enums_and_match() {
    let out = swift(
        "enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\nfunc sum(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Node(v, l, r)\n            return v + sum(l) + sum(r)\n",
    );
    assert!(out.contains("enum Tree"), "{out}");
    assert!(out.contains("case leaf"), "{out}");
    assert!(out.contains("indirect case node"), "{out}");
    assert!(out.contains("case .leaf:"), "{out}");
    assert!(out.contains("case .node(let v, let l, let r):"), "{out}");
}

#[test]
fn option_uses_runtime_types() {
    let out = swift(
        "func f(o: Option<int>) -> int\n    match o\n        case Some(v)\n            return v\n        case None\n            return 0\n",
    );
    assert!(out.contains("SudoOption"), "{out}");
    assert!(out.contains("case .some(let v):"), "{out}");
    assert!(out.contains("case .none:"), "{out}");
}

#[test]
fn tests_emit_with_runner() {
    let out = swift(
        "func double(x: int) -> int\n    return x * 2\ntest \"doubles work\"\n    assert double(2) == 4\n",
    );
    assert!(out.contains("func test_doubles_work() throws"), "{out}");
    assert!(out.contains("runTests"), "{out}");
    assert!(out.contains("@main"), "{out}");
    assert!(out.contains("sudoAssertEq"), "{out}");
}

#[test]
fn library_mode_omits_tests() {
    let src = "func double(x: int) -> int\n    return x * 2\ntest \"t\"\n    assert double(2) == 4\n";
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    let out = emit(&ir, false);
    assert!(!out.contains("func test_"), "{out}");
    assert!(!out.contains("runTests"), "{out}");
    assert!(!out.contains("@main"), "{out}");
}

#[test]
fn text_literals_are_int64_arrays() {
    let out = swift("func f() -> text\n    return \"abc\"\n");
    assert!(out.contains("Int64(97)"), "{out}");
    assert!(out.contains("Int64(98)"), "{out}");
    assert!(out.contains("Int64(99)"), "{out}");
}

#[test]
fn ints_emit_as_int64() {
    let out = swift("func f() -> int\n    return 9223372036854775807\n");
    assert!(out.contains("Int64(9223372036854775807)"), "{out}");
}

#[test]
fn tuples_become_named_structs() {
    let out = swift("func f() -> (int, int)\n    return (1, 2)\n");
    assert!(out.contains("struct Tup2_3i64_3i64"), "{out}");
    assert!(out.contains("Tup2_3i64_3i64("), "{out}");
}

#[test]
fn not_binds_tighter_than_equality() {
    // `not (a == b)` must be `!(a == b)`, never `!a == b`.
    let out = swift("func f(a: int, b: int) -> bool\n    return not (a == b)\n");
    assert!(out.contains("!(a == b)"), "expected parenthesized equality under not, got:\n{out}");
    assert!(!out.contains("!a == b"), "{out}");
}

#[test]
fn for_range_uses_sudo_range() {
    let out = swift(
        "func f() -> int\n    s = 0\n    for i = 1 to 3\n        s = s + i\n    return s\n",
    );
    assert!(out.contains("sudoRange"), "{out}");
}
