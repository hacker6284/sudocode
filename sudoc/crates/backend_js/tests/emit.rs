use sudoc_backend_js::emit;

fn js(src: &str) -> String {
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    emit(&ir, true)
}

#[test]
fn gcd_reads_like_js() {
    let out = js(
        "export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n",
    );
    assert!(out.contains("export function gcd(a, b) {"), "{out}");
    assert!(out.contains("while (b !== 0n) {"), "{out}");
    // mod is floor modulo with a zero-divisor trap — via the runtime.
    assert!(out.contains("_rt.mod_i64(a, b)"), "{out}");
    // abs on int wraps at i64 min — via the runtime.
    assert!(out.contains("_rt.abs_i64(a)"), "{out}");
}

#[test]
fn int_arithmetic_is_checked() {
    let out = js("func f(a: int, b: int) -> int\n    return a * b + 1\n");
    assert!(out.contains("_rt.chk("), "{out}");
    assert!(out.contains("1n"), "{out}");
}

#[test]
fn float_arithmetic_is_plain() {
    let out = js("func f(a: float, b: float) -> float\n    return a * b + 1.0\n");
    assert!(!out.contains("_rt.chk("), "{out}");
    assert!(out.contains("a * b + 1.0"), "{out}");
}

#[test]
fn single_inout_writeback() {
    let out = js(
        "func bump(x: inout int)\n    x = x + 1\nfunc f() -> int\n    n = 0\n    bump(n)\n    return n\n",
    );
    // Callee returns the inout value; call site reassigns it.
    assert!(out.contains("return x;"), "{out}");
    assert!(out.contains("n = bump(n)"), "{out}");
}

#[test]
fn ret_plus_inout_unpacks() {
    let out = js(
        "func take(x: inout int) -> int\n    x = x + 1\n    return x * 2\nfunc f() -> int\n    n = 0\n    y = take(n)\n    return y + n\n",
    );
    assert!(out.contains("[y, n] = take(n)"), "{out}");
}

#[test]
fn composite_params_are_copied_on_entry() {
    let out = js("func f(items: List<int>) -> int\n    return items.length\n");
    assert!(out.contains("items = _rt.dup(items)"), "{out}");
    // ...but inout params are not.
    let out2 = js("func g(items: inout List<int>)\n    items.append(1)\n");
    assert!(!out2.contains("_rt.dup(items)"), "{out2}");
    assert!(out2.contains("items.push("), "{out2}");
}

#[test]
fn aliasing_assignment_copies() {
    let out = js("func f(a: List<int>) -> List<int>\n    b = a\n    b.append(1)\n    return b\n");
    assert!(out.contains("b = _rt.dup(a)"), "{out}");
    // Scalars never copy.
    let out2 = js("func g(x: int) -> int\n    y = x\n    return y\n");
    assert!(!out2.contains("dup"), "{out2}");
}

#[test]
fn deep_equality_via_runtime() {
    let out = js("func f(a: List<int>, b: List<int>) -> bool\n    return a == b\n");
    assert!(out.contains("_rt.eq(a, b)"), "{out}");
    let out2 = js("func g(x: int, y: int) -> bool\n    return x == y\n");
    assert!(out2.contains("x === y"), "{out2}");
}

#[test]
fn enums_become_classes_and_match() {
    let out = js(
        "enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\nfunc sum(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Node(v, l, r)\n            return v + sum(l) + sum(r)\n",
    );
    assert!(out.contains("export class Tree_Leaf"), "{out}");
    assert!(out.contains("export class Tree_Node"), "{out}");
    assert!(out.contains("instanceof Tree_Leaf"), "{out}");
    assert!(out.contains("instanceof Tree_Node"), "{out}");
}

#[test]
fn option_uses_runtime_types() {
    let out = js(
        "func f(o: Option<int>) -> int\n    match o\n        case Some(v)\n            return v\n        case None\n            return 0\n",
    );
    assert!(out.contains("instanceof _rt.Some"), "{out}");
    assert!(out.contains("instanceof _rt.NoneOpt"), "{out}");
}

#[test]
fn tests_emit_with_runner() {
    let out = js(
        "func double(x: int) -> int\n    return x * 2\ntest \"doubles work\"\n    assert double(2) == 4\n",
    );
    assert!(out.contains("function test_doubles_work()"), "{out}");
    assert!(out.contains("_rt.run_tests"), "{out}");
    assert!(out.contains("_rt.sudo_assert_eq("), "{out}");
}

#[test]
fn library_mode_omits_tests() {
    let src = "func double(x: int) -> int\n    return x * 2\ntest \"t\"\n    assert double(2) == 4\n";
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    let out = emit(&ir, false);
    assert!(!out.contains("function test_"), "{out}");
    assert!(!out.contains("run_tests"), "{out}");
}

#[test]
fn text_literals_are_bigint_arrays() {
    let out = js("func f() -> text\n    return \"abc\"\n");
    // 'a'=97, 'b'=98, 'c'=99
    assert!(out.contains("97n"), "{out}");
    assert!(out.contains("98n"), "{out}");
    assert!(out.contains("99n"), "{out}");
}

#[test]
fn ints_emit_as_bigint_literals() {
    let out = js("func f() -> int\n    return 9223372036854775807\n");
    assert!(out.contains("9223372036854775807n"), "{out}");
}
