use sudoc_backend_rs::emit;

fn rs(src: &str) -> String {
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    emit(&ir, true, true)
}

#[test]
fn gcd_reads_like_rust() {
    let out = rs(
        "export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n",
    );
    assert!(out.contains("pub(crate) fn gcd(mut a: i64, mut b: i64) -> i64"), "{out}");
    assert!(out.contains("while b != 0i64"), "{out}");
    assert!(out.contains("crate::sudo_rt::mod_i64"), "{out}");
    assert!(out.contains("crate::sudo_rt::abs_i64"), "{out}");
}

#[test]
fn int_arithmetic_is_checked() {
    let out = rs("func f(a: int, b: int) -> int\n    return a * b + 1\n");
    assert!(out.contains("crate::sudo_rt::chk_mul"), "{out}");
    assert!(out.contains("crate::sudo_rt::chk_add"), "{out}");
    assert!(out.contains("1i64"), "{out}");
}

#[test]
fn float_arithmetic_is_plain() {
    let out = rs("func f(a: float, b: float) -> float\n    return a * b + 1.0\n");
    assert!(!out.contains("chk_"), "{out}");
    assert!(out.contains("a * b"), "{out}");
    assert!(out.contains("1.0"), "{out}");
}

#[test]
fn single_inout_uses_mut_ref() {
    let out = rs(
        "func bump(x: inout int)\n    x = x + 1\nfunc f() -> int\n    n = 0\n    bump(n)\n    return n\n",
    );
    assert!(out.contains("x: &mut i64"), "{out}");
    assert!(out.contains("bump(&mut n)"), "{out}");
    assert!(out.contains("*x = "), "{out}");
}

#[test]
fn ret_plus_inout_no_tuple_writeback() {
    let out = rs(
        "func take(x: inout int) -> int\n    x = x + 1\n    return x * 2\nfunc f() -> int\n    n = 0\n    y = take(n)\n    return y + n\n",
    );
    // Native &mut — no [y, n] = take(n) unpacking.
    assert!(out.contains("take(&mut n)"), "{out}");
    assert!(!out.contains("[y, n]"), "{out}");
}

#[test]
fn composite_params_cloned_at_call_not_entry() {
    let out = rs(
        "func f(items: List<int>) -> int\n    return items.length\nfunc g() -> int\n    a = [1, 2]\n    return f(a)\n",
    );
    // Caller clones aliasing composite args (Rust move safety).
    assert!(out.contains("f((a).clone())") || out.contains("f(a.clone())"), "{out}");
    // Inout params are &mut, not cloned at call for the ref itself.
    let out2 = rs("func g(items: inout List<int>)\n    items.append(1)\n");
    assert!(out2.contains("items: &mut Vec<i64>"), "{out2}");
    assert!(out2.contains(".push("), "{out2}");
}

#[test]
fn aliasing_assignment_copies() {
    let out = rs("func f(a: List<int>) -> List<int>\n    b = a\n    b.append(1)\n    return b\n");
    assert!(out.contains("(a).clone()") || out.contains("a.clone()"), "{out}");
    let out2 = rs("func g(x: int) -> int\n    y = x\n    return y\n");
    assert!(!out2.contains("clone"), "{out2}");
}

#[test]
fn deep_equality_is_native() {
    let out = rs("func f(a: List<int>, b: List<int>) -> bool\n    return a == b\n");
    assert!(out.contains("a == b"), "{out}");
    let out2 = rs("func g(x: int, y: int) -> bool\n    return x == y\n");
    assert!(out2.contains("x == y"), "{out2}");
}

#[test]
fn enums_become_rust_enums_and_match() {
    let out = rs(
        "enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\nfunc sum(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Node(v, l, r)\n            return v + sum(l) + sum(r)\n",
    );
    assert!(out.contains("pub(crate) enum Tree"), "{out}");
    assert!(out.contains("Leaf"), "{out}");
    assert!(out.contains("Node"), "{out}");
    assert!(out.contains("Box<Tree>"), "{out}");
    assert!(out.contains("match _sudo_sc"), "{out}");
    assert!(out.contains("Tree::Leaf"), "{out}");
    assert!(out.contains("Tree::Node"), "{out}");
}

#[test]
fn option_uses_native_option() {
    let out = rs(
        "func f(o: Option<int>) -> int\n    match o\n        case Some(v)\n            return v\n        case None\n            return 0\n",
    );
    assert!(out.contains("Some("), "{out}");
    assert!(out.contains("None"), "{out}");
    assert!(out.contains("Option<i64>") || out.contains("mut o: Option"), "{out}");
}

#[test]
fn tests_emit_with_runner() {
    let out = rs(
        "func double(x: int) -> int\n    return x * 2\ntest \"doubles work\"\n    assert double(2) == 4\n",
    );
    assert!(out.contains("fn test_doubles_work()"), "{out}");
    assert!(out.contains("crate::sudo_rt::run_tests"), "{out}");
    assert!(out.contains("crate::sudo_rt::sudo_assert_eq"), "{out}");
    assert!(out.contains("fn main()"), "{out}");
}

#[test]
fn library_mode_omits_tests() {
    let src = "func double(x: int) -> int\n    return x * 2\ntest \"t\"\n    assert double(2) == 4\n";
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    let out = emit(&ir, false, true);
    assert!(!out.contains("fn test_"), "{out}");
    assert!(!out.contains("run_tests"), "{out}");
    assert!(!out.contains("fn main()"), "{out}");
}

#[test]
fn text_literals_use_runtime_helper() {
    let out = rs("func f() -> text\n    return \"abc\"\n");
    assert!(out.contains("crate::sudo_rt::text"), "{out}");
    assert!(out.contains("97i64"), "{out}");
    assert!(out.contains("98i64"), "{out}");
    assert!(out.contains("99i64"), "{out}");
}

#[test]
fn ints_emit_as_i64_literals() {
    let out = rs("func f() -> int\n    return 9223372036854775807\n");
    assert!(out.contains("9223372036854775807i64"), "{out}");
}

#[test]
fn for_range_uses_i128_cursor() {
    let out = rs("func f() -> int\n    s = 0\n    for i = 1 to 3\n        s = s + i\n    return s\n");
    assert!(out.contains("as i128"), "{out}");
    assert!(out.contains("_sudo_from_i"), "{out}");
    assert!(out.contains("_sudo_to_i"), "{out}");
}
