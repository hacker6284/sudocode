use sudoc_ir::wire::to_wire_json;
use sudoc_ir::{IrExprKind, IrStmt};
use sudoc_types::termination::{FuncFact, FuncId};
use sudoc_types::{check_program_files, check_source, Program, TypeError};

fn prog(src: &str) -> Program {
    check_program_files(&[("m", src)]).unwrap_or_else(|es| panic!("check failed: {es:?}"))
}

fn fact<'a>(p: &'a Program, name: &str) -> &'a FuncFact {
    p.termination
        .funcs
        .get(&FuncId {
            module: "m".into(),
            name: name.into(),
        })
        .unwrap_or_else(|| panic!("missing termination fact for {name}"))
}

fn type_err(src: &str) -> TypeError {
    match check_source(src, "m") {
        Err(mut es) => es.remove(0),
        Ok(_) => panic!("expected a type error"),
    }
}

fn count_whiles(stmts: &[IrStmt]) -> usize {
    stmts
        .iter()
        .map(|s| match s {
            IrStmt::While { body, .. } => 1 + count_whiles(body),
            IrStmt::If { arms, else_block } => {
                arms.iter().map(|(_, b)| count_whiles(b)).sum::<usize>()
                    + else_block.as_ref().map(|b| count_whiles(b)).unwrap_or(0)
            }
            IrStmt::ForRange { body, .. } | IrStmt::ForIn { body, .. } => count_whiles(body),
            IrStmt::Match { arms, .. } => arms.iter().map(|a| count_whiles(&a.body)).sum(),
            IrStmt::ExpectTrap { body, .. } => count_whiles(body),
            _ => 0,
        })
        .sum()
}

#[test]
fn proved_decreases_erases_and_accepts_count() {
    let with = "\
func count(n: int) -> int decreases n
    if n <= 0
        return 0
    return count(n - 1) + 1
";
    let without = "\
func count(n: int) -> int
    if n <= 0
        return 0
    return count(n - 1) + 1
";
    let annotated = prog(with);
    assert!(
        fact(&annotated, "count").direct.is_none(),
        "{:?}",
        fact(&annotated, "count").direct
    );
    let a = check_source(with, "m").expect("annotated checks");
    let b = check_source(without, "m").expect("unannotated still checks");
    assert_eq!(
        to_wire_json(&[a]).unwrap(),
        to_wire_json(&[b]).unwrap(),
        "proved decreases must erase to the same modules JSON"
    );
    let plain = prog(without);
    let refusal = fact(&plain, "count")
        .direct
        .as_ref()
        .expect("unannotated recursion is a refusal");
    assert_eq!(refusal.predicate, "terminates");
    assert!(
        refusal.reason.contains("not structural"),
        "{}",
        refusal.reason
    );
}

#[test]
fn increasing_while_measure_is_type_error() {
    let src = "\
func spin(n: int)
    while true decreases n
        n = n + 1
";
    let e = type_err(src);
    assert_eq!(e.msg, "decreases measure does not decrease");
    assert_eq!(
        e.to_string(),
        format!("{}:{}: decreases measure does not decrease", e.line, e.col)
    );
    assert_eq!((e.line, e.col), (2, 5));
}

#[test]
fn bad_measure_shape_is_type_error() {
    let arith = type_err("func f(n: int) -> int decreases n - 1\n    return n\n");
    assert_eq!(
        arith.msg,
        "decreases measure must be an int parameter, or .length / .size of a parameter"
    );
    let float = type_err("func f(x: float) -> float decreases x\n    return x\n");
    assert_eq!(
        float.msg,
        "decreases measure must be an int parameter, or .length / .size of a parameter"
    );
    let unknown = type_err("func f(n: int) -> int decreases m\n    return n\n");
    assert!(
        unknown.msg.contains("unknown"),
        "ill-typed measure stays an ordinary type error: {}",
        unknown.msg
    );
}

#[test]
fn set_remove_and_map_delete_do_not_shrink_size() {
    let set = type_err(
        "\
func f(s: Set<int>)
    while true decreases s.size
        s.remove(0)
",
    );
    assert_eq!(set.msg, "decreases measure does not decrease");
    let map = type_err(
        "\
func f(m: Map<int, int>)
    while true decreases m.size
        m.delete(0)
",
    );
    assert_eq!(map.msg, "decreases measure does not decrease");
}

#[test]
fn list_pop_decreases_length() {
    let p = prog(
        "\
func drain(xs: List<int>) -> int
    while xs.length > 0 decreases xs.length
        xs.pop()
    return 0
",
    );
    assert!(fact(&p, "drain").direct.is_none());
}

#[test]
fn one_arm_and_nested_for_do_not_prove_decreases() {
    let one_arm = type_err(
        "\
func f(n: int)
    while true decreases n
        if n > 0
            n = n - 1
",
    );
    assert_eq!(one_arm.msg, "decreases measure does not decrease");
    let nested = type_err(
        "\
func f(n: int)
    while true decreases n
        for i = 1 to 3
            n = n - 1
",
    );
    assert_eq!(nested.msg, "decreases measure does not decrease");
}

#[test]
fn rose_size_on_element_is_accepted() {
    let p = prog(
        "\
enum Rose
    Node(kids: List<Rose>)

func size(t: Rose) -> int
    match t
        case Node(kids)
            s = 0
            for k in kids
                s = s + size(k)
            return s
",
    );
    assert!(
        fact(&p, "size").direct.is_none(),
        "{:?}",
        fact(&p, "size").direct
    );
}

#[test]
fn passing_the_rose_container_is_not_descent() {
    let p = prog(
        "\
enum Rose
    Node(kids: List<Rose>)

func size(t: Rose) -> int
    match t
        case Node(kids)
            return sum(kids)

func sum(xs: List<Rose>) -> int
    s = 0
    for k in xs
        s = s + size(k)
    return s
",
    );
    let size = fact(&p, "size")
        .direct
        .as_ref()
        .expect("size(kids) is not descent");
    assert_eq!(size.predicate, "terminates");
    assert!(size.reason.contains("not structural"), "{}", size.reason);
}

#[test]
fn bst_insert_is_accepted() {
    let p = prog(
        "\
enum Tree
    Leaf
    Node(value: int, left: Tree, right: Tree)

func insert(t: Tree, v: int) -> Tree
    match t
        case Leaf
            return Node(v, Leaf, Leaf)
        case Node(value, left, right)
            if v < value
                return Node(value, insert(left, v), right)
            else if v > value
                return Node(value, left, insert(right, v))
            else
                return Node(value, left, right)
",
    );
    assert!(
        fact(&p, "insert").direct.is_none(),
        "{:?}",
        fact(&p, "insert").direct
    );
}

#[test]
fn same_parameter_is_not_structural_descent() {
    let p = prog(
        "\
enum Tree
    Leaf
    Node(value: int, left: Tree, right: Tree)

func id(t: Tree) -> Tree
    match t
        case Leaf
            return Leaf
        case Node(value, left, right)
            return id(t)
",
    );
    let r = fact(&p, "id").direct.as_ref().expect("refusal");
    assert_eq!(r.predicate, "terminates");
    assert!(r.reason.contains("not structural"), "{}", r.reason);
}

#[test]
fn hoisted_inout_condition_while_respects_body_measure() {
    let ok_src = "\
func touch(p: inout int) -> bool
    return true

func dec(n: int) -> int
    p = 0
    while touch(p) decreases n
        n = n - 1
    return n
";
    let p = prog(ok_src);
    assert!(
        fact(&p, "dec").direct.is_none(),
        "{:?}",
        fact(&p, "dec").direct
    );
    let m = check_source(ok_src, "m").expect("checks");
    let dec = m.func("dec").unwrap();
    assert_eq!(count_whiles(&dec.body), 1);
    let IrStmt::While { cond, .. } = &dec
        .body
        .iter()
        .find(|s| matches!(s, IrStmt::While { .. }))
        .unwrap()
    else {
        unreachable!("while")
    };
    assert!(
        matches!(cond.kind, IrExprKind::Bool(true)),
        "inout condition must be the hoisted while-true image, got {:?}",
        cond.kind
    );

    let bad = type_err(
        "\
func touch(p: inout int) -> bool
    return true

func inc(n: int) -> int
    p = 0
    while touch(p) decreases n
        n = n + 1
    return n
",
    );
    assert_eq!(bad.msg, "decreases measure does not decrease");
}

#[test]
fn unannotated_while_is_refusal() {
    // Not executed: the body does not terminate. Checking it must still succeed.
    let p = prog(
        "\
func spin()
    while true
        skip
",
    );
    let r = fact(&p, "spin").direct.as_ref().expect("refusal");
    assert_eq!(r.predicate, "terminates");
    assert_eq!(r.reason, "while has no decreases measure");
    assert_eq!(r.line, 2);
}

#[test]
fn quicksort_shaped_recursion_is_refusal() {
    let p = prog(
        "\
func quicksort_range(items: inout List<int>, lo: int, hi: int)
    if lo < hi
        quicksort_range(items, lo, hi - 1)
",
    );
    let r = fact(&p, "quicksort_range")
        .direct
        .as_ref()
        .expect("refusal");
    assert_eq!(r.predicate, "terminates");
    assert!(r.reason.contains("not structural"), "{}", r.reason);
    assert!(check_source(
        "\
func quicksort_range(items: inout List<int>, lo: int, hi: int)
    if lo < hi
        quicksort_range(items, lo, hi - 1)
",
        "m"
    )
    .is_ok());
}

#[test]
fn indirect_call_is_refusal() {
    let p = prog(
        "\
func sort_by(items: List<int>, less: func(int, int) -> bool) -> bool
    return less(items[0], items[1])
",
    );
    let r = fact(&p, "sort_by").direct.as_ref().expect("refusal");
    assert_eq!(r.predicate, "terminates");
    assert_eq!(r.reason, "indirect call; terminates cannot see the callee");
}

#[test]
fn func_ref_local_called_once_is_direct() {
    let p = prog(
        "\
func id(x: int) -> int
    return x

func apply(x: int) -> int
    f = id
    return f(x)
",
    );
    assert!(
        fact(&p, "apply").direct.is_none(),
        "{:?}",
        fact(&p, "apply").direct
    );
    assert!(fact(&p, "apply")
        .calls
        .iter()
        .any(|c| c.callee.name == "id"));
}

#[test]
fn even_odd_decreases_are_accepted_without_are_refusals() {
    let proved = prog(
        "\
func even(n: int) -> bool decreases n
    if n == 0
        return true
    return odd(n - 1)

func odd(n: int) -> bool decreases n
    if n == 0
        return false
    return even(n - 1)
",
    );
    assert!(fact(&proved, "even").direct.is_none());
    assert!(fact(&proved, "odd").direct.is_none());

    let plain = prog(
        "\
func even(n: int) -> bool
    if n == 0
        return true
    return odd(n - 1)

func odd(n: int) -> bool
    if n == 0
        return false
    return even(n - 1)
",
    );
    assert!(fact(&plain, "even").direct.is_some());
    assert!(fact(&plain, "odd").direct.is_some());
    assert!(check_source(
        "\
func even(n: int) -> bool
    if n == 0
        return true
    return odd(n - 1)

func odd(n: int) -> bool
    if n == 0
        return false
    return even(n - 1)
",
        "m"
    )
    .is_ok());
}

#[test]
fn full_i64_range_for_is_accepted_without_counter() {
    let src = "\
func big()
    for i = -9223372036854775808 to 9223372036854775807
        skip
";
    let p = prog(src);
    assert!(
        fact(&p, "big").direct.is_none(),
        "{:?}",
        fact(&p, "big").direct
    );
    let m = check_source(src, "m").unwrap();
    let f = m.func("big").unwrap();
    assert_eq!(f.body.len(), 1, "no synthesized counter statement");
    match &f.body[0] {
        IrStmt::ForRange {
            from,
            to,
            down,
            body,
            ..
        } => {
            assert!(!down);
            assert!(matches!(from.kind, IrExprKind::Int(v) if v == i64::MIN));
            assert!(matches!(to.kind, IrExprKind::Int(v) if v == i64::MAX));
            assert!(matches!(body.as_slice(), [IrStmt::Skip]));
        }
        other => panic!("expected ForRange, got {other:?}"),
    }
}

#[test]
fn negated_literal_add_and_inout_int_measure_are_accepted() {
    let add = prog(
        "\
func count(n: int) -> int decreases n
    if n <= 0
        return 0
    return count(n + (-1)) + 1
",
    );
    assert!(
        fact(&add, "count").direct.is_none(),
        "{:?}",
        fact(&add, "count").direct
    );

    let inout = prog(
        "\
func down(n: inout int) decreases n
    if n <= 0
        return
    n = n - 1
    down(n)
",
    );
    assert!(
        fact(&inout, "down").direct.is_none(),
        "{:?}",
        fact(&inout, "down").direct
    );
}
