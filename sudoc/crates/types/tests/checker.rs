use sudoc_ir::{IrExprKind, IrModule, Ty};
use sudoc_types::check_source;

fn ok(src: &str) -> IrModule {
    match check_source(src, "m") {
        Ok(m) => m,
        Err(es) => panic!("expected to check, got errors: {es:?}"),
    }
}

/// First error message for a source that must fail.
fn err(src: &str) -> String {
    match check_source(src, "m") {
        Ok(_) => panic!("expected a type error"),
        Err(es) => es[0].msg.clone(),
    }
}

// ---- happy paths ----------------------------------------------------------

#[test]
fn gcd_checks_and_lowers() {
    let m = ok("export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n");
    let f = m.func("gcd").expect("gcd in IR");
    assert!(f.export);
    assert_eq!(f.ret, Some(Ty::Int));
    assert_eq!(f.params[0].ty, Ty::Int);
}

#[test]
fn inference_from_first_assignment() {
    ok("func f() -> float\n    x = 1.5\n    x = x + 1.0\n    return x\n");
}

#[test]
fn empty_list_inferred_from_use() {
    let m = ok("func f() -> List<int>\n    items = []\n    items.append(3)\n    return items\n");
    let f = m.func("f").unwrap();
    // The declaration must carry the resolved type.
    match &f.body[0] {
        sudoc_ir::IrStmt::Assign { value, declares, .. } => {
            assert!(*declares);
            assert_eq!(value.ty, Ty::list(Ty::Int));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn text_is_list_of_int() {
    ok("func f() -> bool\n    return \"abc\" == [97, 98, 99]\n");
    let m = ok("export func rev(s: text) -> text\n    return s\n");
    let f = m.func("rev").unwrap();
    assert_eq!(f.params[0].ty, Ty::list(Ty::Int)); // erased
    assert_eq!(
        f.params[0].boundary,
        sudoc_ir::BoundaryTy::Text // boundary intent survives
    );
}

#[test]
fn option_typing() {
    ok("func find(items: List<int>, t: int) -> Option<int>\n    for i = 0 to items.length - 1\n        if items[i] == t\n            return Some(i)\n    return None\n");
    ok("func f(o: Option<int>) -> int\n    return o.get_or(0)\n");
    ok("func g(o: Option<int>) -> int\n    if o.is_some()\n        return o.unwrap()\n    return -1\n");
}

#[test]
fn map_and_set_ops() {
    ok("func f() -> int\n    m = Map()\n    m[1] = 10\n    s = Set()\n    s.add(m[1])\n    keys = m.keys()\n    keys.sort()\n    return m.size + s.size + keys[0]\n");
}

#[test]
fn map_get_returns_option() {
    ok("func f(m: Map<int, int>) -> int\n    return m.get(3).get_or(0)\n");
}

#[test]
fn tuple_assign_and_parallel_assign() {
    ok("func mm(a: int, b: int) -> (int, int)\n    return (a, b)\n");
    ok("func f() -> int\n    x, y = 1, 2\n    x, y = y, x\n    return x + y\n");
    ok("func mm(a: int) -> (int, int)\n    return (a, a)\n func_sep\n".replace(" func_sep\n", "\n").as_str());
    ok("func mm(a: int) -> (int, int)\n    return (a, a)\nfunc f() -> int\n    lo, hi = mm(3)\n    return lo + hi\n");
}

#[test]
fn records_construct_access_mutate() {
    let src = "record Point\n    x: int\n    y: int\nfunc f() -> int\n    p = Point(1, 2)\n    q = Point(x = 3, y = 4)\n    p.x = p.x + q.y\n    return p.x\n";
    let m = ok(src);
    assert_eq!(m.records[0].name, "Point");
}

#[test]
fn recursive_record_direct_rejected() {
    let e = err("record R\n    next: R\n    v: int\n");
    assert!(e.contains("infinite size"), "{e}");
    assert!(e.contains("R"), "{e}");

    // Indirect by-value cycle through a Tuple field is equally uninhabitable.
    let e2 = err("record R\n    next: (R, int)\n    v: int\n");
    assert!(e2.contains("infinite size"), "{e2}");
}

#[test]
fn recursive_record_via_option_and_list_accepted() {
    // The useful, common forms: heap/pointer indirection breaks the cycle
    // and MUST stay legal.
    ok("record Person\n    name: text\n    partner: Option<Person>\n");
    ok("record Person\n    name: text\n    friends: List<Person>\n");
    ok("record Person\n    name: text\n    partner: Option<Person>\n    friends: List<Person>\n");
}

#[test]
fn enums_and_match() {
    let src = "enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\nfunc sum(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Node(v, l, r)\n            return v + sum(l) + sum(r)\n";
    let m = ok(src);
    assert_eq!(m.enums[0].variants.len(), 2);
}

#[test]
fn qualified_variant_construction() {
    ok("enum A\n    Red\n    Blue\nfunc f() -> A\n    x = A.Red\n    return x\n");
    ok("enum E\n    V(a: int)\nfunc f() -> E\n    return E.V(3)\n");
    let e = err("enum A\n    Red\n    Blue\nfunc f() -> A\n    return A.Green\n");
    assert!(e.contains("Green"), "{e}");
}

#[test]
fn ambiguous_shared_variant_requires_qualification() {
    let src_defs = "enum A\n    Red\n    Blue\nenum B\n    Red\n    Green\n";
    let e = err(&format!("{src_defs}func f() -> A\n    return Red\n"));
    assert!(e.contains("ambiguous"), "{e}");
    ok(&format!("{src_defs}func f() -> A\n    x = A.Red\n    return x\n"));
}

#[test]
fn match_on_int_with_wildcard() {
    ok("func f(x: int) -> int\n    match x\n        case 0\n            return 10\n        case _\n            return 20\n");
}

#[test]
fn inout_params() {
    ok("func bump(x: inout int)\n    x = x + 1\nfunc f() -> int\n    n = 0\n    bump(n)\n    return n\n");
    ok("func fill(items: inout List<int>)\n    items.append(1)\n");
}

#[test]
fn function_reference_params() {
    ok("func is_less(a: int, b: int) -> bool\n    return a < b\nfunc apply(f: func(int, int) -> bool, x: int) -> bool\n    return f(x, x)\nfunc g() -> bool\n    return apply(is_less, 1)\n");
}

#[test]
fn module_consts() {
    ok("limit = 100\nfunc f() -> int\n    return limit\n");
}

#[test]
fn composite_module_const_list() {
    let m = ok("xs = [1, 2, 3]\nfunc f() -> List<int>\n    return xs\n");
    let c = m.consts.iter().find(|c| c.name == "xs").expect("xs const");
    assert!(matches!(c.ty, Ty::List(_)));
    assert!(matches!(&c.value.kind, IrExprKind::List(items) if items.len() == 3));
}

#[test]
fn composite_module_const_empty_map_with_annotation() {
    ok("m: Map<int, int> = Map()\nfunc f() -> Map<int, int>\n    return m\n");
}

#[test]
fn composite_module_const_tuple() {
    ok("t = (1, true)\nfunc f() -> (int, bool)\n    return t\n");
}

#[test]
fn composite_module_const_record() {
    ok("record Point\n    x: int\n    y: int\np = Point(1, 2)\nfunc f() -> Point\n    return p\n");
}

#[test]
fn composite_module_const_refers_to_scalar_const() {
    // Scalar refs are inlined into the fold; list elements that are scalar
    // constants become Int literals after folding. Composite-to-composite
    // refs stay as Const(...).
    let m = ok("a = 5\nb = [a, 10]\nfunc f() -> List<int>\n    return b\n");
    let b = m.consts.iter().find(|c| c.name == "b").expect("b const");
    match &b.value.kind {
        IrExprKind::List(items) => {
            assert_eq!(items.len(), 2);
            // `a` is scalar so it is inlined as Int(5), not Const("a").
            assert!(matches!(&items[0].kind, IrExprKind::Int(5)), "{:?}", items[0].kind);
            assert!(matches!(&items[1].kind, IrExprKind::Int(10)), "{:?}", items[1].kind);
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn composite_module_const_refers_to_composite_const() {
    let m = ok("base = [1, 2]\nxs = [base]\nfunc f() -> List<List<int>>\n    return xs\n");
    let xs = m.consts.iter().find(|c| c.name == "xs").expect("xs const");
    match &xs.value.kind {
        IrExprKind::List(items) => {
            assert_eq!(items.len(), 1);
            assert!(
                matches!(&items[0].kind, IrExprKind::Const(n) if n == "base"),
                "{:?}",
                items[0].kind
            );
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn module_const_rejects_function_call() {
    let e = err("x = some_undefined_func()\n");
    assert!(
        e.contains("not a constant expression") || e.contains("some_undefined_func"),
        "{e}"
    );
}

#[test]
fn module_const_empty_list_needs_annotation() {
    let e = err("xs = []\n");
    assert!(e.to_lowercase().contains("infer"), "{e}");
}

#[test]
fn cannot_mutate_composite_module_const() {
    let e = err("base = [1, 2, 3]\nfunc f()\n    base.append(4)\n");
    assert!(
        e.contains("cannot mutate module constant 'base'"),
        "{e}"
    );
}

#[test]
fn examples_all_check() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../examples");
    let mut checked = 0;
    for entry in walk(dir.as_ref()) {
        let src = std::fs::read_to_string(&entry).unwrap();
        let name = entry.file_stem().unwrap().to_str().unwrap().to_string();
        if let Err(es) = check_source(&src, &name) {
            panic!("{} failed to check: {}", entry.display(), es[0]);
        }
        checked += 1;
    }
    assert!(checked >= 9, "expected at least 9 examples, found {checked}");
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

// ---- errors ---------------------------------------------------------------

#[test]
fn undeclared_variable() {
    let e = err("func f() -> int\n    return x\n");
    assert!(e.contains("x"), "{e}");
}

#[test]
fn no_implicit_numeric_mixing() {
    let e = err("func f() -> float\n    return 1 + 2.0\n");
    assert!(e.contains("int") && e.contains("float"), "{e}");
}

#[test]
fn reassignment_must_keep_type() {
    let e = err("func f()\n    x = 1\n    x = 1.5\n");
    assert!(e.contains("int") && e.contains("float"), "{e}");
}

#[test]
fn unconstrained_empty_list() {
    let e = err("func f()\n    items = []\n");
    assert!(e.to_lowercase().contains("infer"), "{e}");
}

#[test]
fn condition_must_be_bool() {
    let e = err("func f(x: int)\n    if x\n        skip\n");
    assert!(e.contains("bool"), "{e}");
}

#[test]
fn loop_var_not_assignable() {
    let e = err("func f()\n    for i = 0 to 3\n        i = 5\n");
    assert!(e.contains("loop"), "{e}");
}

#[test]
fn missing_return_on_some_path() {
    let e = err("func f(x: int) -> int\n    if x > 0\n        return 1\n");
    assert!(e.to_lowercase().contains("return"), "{e}");
}

#[test]
fn while_true_with_return_satisfies_definite_return() {
    ok("func f() -> int\n    while true\n        return 5\n");
    ok("func f(x: int) -> int\n    while true\n        if x > 0\n            return x\n        x = x + 1\n");
}

#[test]
fn while_true_with_break_still_requires_return() {
    // A reachable `break` means the loop can fall through, so this must
    // still be rejected — do not regress this direction while fixing the
    // `while true` + `return` case above.
    let e = err("func f() -> int\n    while true\n        break\n");
    assert!(e.to_lowercase().contains("return"), "{e}");

    let e = err("func f(x: bool) -> int\n    while true\n        if x\n            break\n        return 1\n");
    assert!(e.to_lowercase().contains("return"), "{e}");
}

#[test]
fn exhaustive_match_counts_as_return() {
    ok("enum Sign\n    Neg\n    Zero\n    Pos\nfunc f(s: Sign) -> int\n    match s\n        case Neg\n            return -1\n        case Zero\n            return 0\n        case Pos\n            return 1\n");
}

#[test]
fn non_exhaustive_match_is_error() {
    let e = err("enum Sign\n    Neg\n    Zero\n    Pos\nfunc f(s: Sign)\n    match s\n        case Neg\n            skip\n");
    assert!(e.to_lowercase().contains("exhaustive") || e.contains("Zero"), "{e}");
}

#[test]
fn match_arity_checked() {
    let e = err("enum E\n    V(a: int, b: int)\nfunc f(e: E)\n    match e\n        case V(x)\n            skip\n");
    assert!(e.contains("2") || e.to_lowercase().contains("arity"), "{e}");
}

#[test]
fn int_match_requires_wildcard() {
    let e = err("func f(x: int)\n    match x\n        case 0\n            skip\n");
    assert!(e.to_lowercase().contains("exhaustive") || e.contains("_"), "{e}");
}

#[test]
fn float_is_not_hashable() {
    let e = err("func f(m: Map<float, int>) -> int\n    return m.size\n");
    assert!(e.to_lowercase().contains("hashable") || e.to_lowercase().contains("key"), "{e}");
}

#[test]
fn inout_argument_must_be_a_variable() {
    let e = err(
        "func bump(x: inout int)\n    x = x + 1\nfunc f(a: List<int>)\n    bump(a[0])\n",
    );
    assert!(e.to_lowercase().contains("inout"), "{e}");
}

#[test]
fn same_var_to_two_inout_params_rejected() {
    let e = err(
        "func two(a: inout int, b: inout int)\n    skip\nfunc f()\n    n = 0\n    two(n, n)\n",
    );
    assert!(e.to_lowercase().contains("inout") || e.to_lowercase().contains("alias"), "{e}");
}

#[test]
fn cannot_assign_to_module_const() {
    let e = err("limit = 10\nfunc f()\n    limit = 20\n");
    assert!(e.to_lowercase().contains("const"), "{e}");
}

#[test]
fn arity_and_arg_types_checked() {
    let e = err("func g(a: int) -> int\n    return a\nfunc f() -> int\n    return g(1, 2)\n");
    assert!(e.contains("1") && e.contains("2"), "{e}");
    let e = err("func g(a: int) -> int\n    return a\nfunc f() -> int\n    return g(1.5)\n");
    assert!(e.contains("float") || e.contains("int"), "{e}");
}

#[test]
fn method_on_wrong_type() {
    let e = err("func f(x: int)\n    x.append(1)\n");
    assert!(e.contains("append"), "{e}");
}

#[test]
fn ordering_only_on_numbers() {
    let e = err("func f(a: List<int>, b: List<int>) -> bool\n    return a < b\n");
    assert!(e.contains("<") || e.to_lowercase().contains("order"), "{e}");
}

#[test]
fn mutating_method_needs_mutable_place() {
    let e = err("func g() -> List<int>\n    return [1]\nfunc f()\n    g().append(2)\n");
    assert!(e.to_lowercase().contains("mutat") || e.to_lowercase().contains("variable"), "{e}");
}

#[test]
fn loop_var_is_immutable_even_for_methods() {
    let e = err("func f(xs: List<List<int>>)\n    for x in xs\n        x.append(1)\n");
    assert!(e.to_lowercase().contains("loop"), "{e}");
}

#[test]
fn generic_functions_monomorphize() {
    let m = ok("func id<T>(x: T) -> T\n    return x\nfunc f() -> int\n    return id(3)\n");
    assert!(m.func("sudo_2id__3i64").is_some(), "instantiation missing: {:?}",
        m.funcs.iter().map(|f| f.name.clone()).collect::<Vec<_>>());
    // Uninstantiated templates do not appear in IR.
    assert!(m.func("id").is_none());
}

#[test]
fn generic_two_instantiations() {
    let m = ok("func id<T>(x: T) -> T\n    return x\nfunc f() -> float\n    y = id(3)\n    return id(1.5)\n");
    assert!(m.func("sudo_2id__3i64").is_some());
    assert!(m.func("sudo_2id__3f64").is_some());
}

#[test]
fn generic_with_function_param_and_inout() {
    let src = "func maxi(a: int, b: int) -> bool\n    return a < b\nfunc sort2<T>(items: inout List<T>, less: func(T, T) -> bool)\n    if items.length == 2\n        if less(items[1], items[0])\n            items.swap(0, 1)\nfunc f() -> List<int>\n    xs = [2, 1]\n    sort2(xs, maxi)\n    return xs\n";
    let m = ok(src);
    assert!(m.func("sudo_5sort2__3i64").is_some());
}

#[test]
fn generic_calls_generic() {
    let src = "func id<T>(x: T) -> T\n    return x\nfunc twice<T>(x: T) -> T\n    return id(id(x))\nfunc f() -> int\n    return twice(7)\n";
    let m = ok(src);
    assert!(m.func("sudo_5twice__3i64").is_some());
    assert!(m.func("sudo_2id__3i64").is_some());
}

#[test]
fn generic_type_arg_must_be_inferrable() {
    let e = err("func empty<T>() -> List<T>\n    return []\nfunc f() -> int\n    x = empty()\n    return 0\n");
    assert!(e.to_lowercase().contains("infer"), "{e}");
}

#[test]
fn polymorphic_recursion_rejected() {
    let e = err("func deep<T>(x: T) -> int\n    return deep([x])\nfunc f() -> int\n    return deep(1)\n");
    assert!(e.to_lowercase().contains("instantiation") || e.to_lowercase().contains("recursi"), "{e}");
}

#[test]
fn generic_reference_without_call_rejected() {
    let e = err("func id<T>(x: T) -> T\n    return x\nfunc apply(f: func(int) -> int) -> int\n    return f(1)\nfunc g() -> int\n    return apply(id)\n");
    assert!(e.to_lowercase().contains("generic"), "{e}");
}

#[test]
fn equality_requires_same_type() {
    let e = err("func f() -> bool\n    return 1 == true\n");
    assert!(e.contains("int") && e.contains("bool"), "{e}");
}

#[test]
fn export_functions_must_not_return_nothing_special() {
    // exports with no return type are fine — just checking nothing crashes.
    ok("export func act(items: inout List<int>)\n    items.append(1)\n");
}

#[test]
fn reserved_prefix_rejected() {
    let e = err("func f()\n    _sudo_x = 1\n");
    assert!(e.contains("_sudo_"), "{e}");
}

#[test]
fn inout_calls_may_nest_in_expressions() {
    // The frontend hoists these to statement-level temps (spec §5.2).
    ok("func take(x: inout int) -> int\n    x = x + 1\n    return x\nfunc f() -> int\n    n = 0\n    return take(n) + 1\n");
    ok("func take(x: inout int) -> int\n    x = x + 1\n    return x\nfunc f()\n    n = 0\n    if take(n) > 0\n        skip\n");
    ok("func take(x: inout int) -> int\n    x = x + 1\n    return x\nfunc f()\n    n = 0\n    while take(n) < 3\n        skip\n");
}

#[test]
fn export_boundary_restrictions() {
    // Option<Option<T>> in a return collapses ambiguously at the Python
    // boundary (spec lockstep.md §5.1).
    let e = err("export func f(x: int) -> Option<Option<int>>\n    return Some(Some(x))\n");
    assert!(e.contains("Option<Option"), "{e}");
    // Non-exported functions may do it freely.
    ok("func f(x: int) -> Option<Option<int>>\n    return Some(Some(x))\n");
    // Scalar/text inout cannot be written back into host bindings.
    let e = err("export func f(x: inout int)\n    x = x + 1\n");
    assert!(e.to_lowercase().contains("inout"), "{e}");
    let e = err("export func f(s: inout text)\n    s.append(33)\n");
    assert!(e.to_lowercase().contains("inout"), "{e}");
    // List/record inout exports are fine.
    ok("export func f(items: inout List<int>)\n    items.append(1)\n");
    // Function-typed params cannot cross the host boundary.
    let e = err("export func f(g: func(int) -> int) -> int\n    return g(1)\n");
    assert!(e.to_lowercase().contains("function"), "{e}");
}

#[test]
fn break_continue_placement() {
    ok("func f() -> int\n    total = 0\n    for i = 1 to 5\n        if i == 3\n            break\n        total = total + i\n    return total\n");
    let e = err("func f()\n    break\n");
    assert!(e.contains("break"), "{e}");
    let e = err("func f()\n    if true\n        continue\n");
    assert!(e.contains("continue"), "{e}");
}

#[test]
fn expect_trap_rules() {
    ok("test \"t\"\n    expect_trap DivByZero\n        x = 1 / 0\n");
    // Only in tests.
    let e = err("func f()\n    expect_trap DivByZero\n        x = 1 / 0\n");
    assert!(e.contains("test"), "{e}");
    // Must be last.
    let e = err("test \"t\"\n    expect_trap DivByZero\n        x = 1 / 0\n    assert true\n");
    assert!(e.contains("final"), "{e}");
    // Kind must be real.
    let e = err("test \"t\"\n    expect_trap Kaboom\n        x = 1 / 0\n");
    assert!(e.contains("Kaboom"), "{e}");
    ok("test \"t\"\n    expect_trap AssertFailed\n        assert false\n");
    let e = err("test \"t\"\n    expect_trap StackOverflow\n        assert true\n");
    assert!(e.contains("StackOverflow"), "{e}");
}

#[test]
fn const_overflow_is_compile_error() {
    let e = err("big = 9223372036854775807 + 1\n");
    assert!(e.to_lowercase().contains("overflow"), "{e}");
    ok("fine = 9223372036854775807 - 1\nfunc f() -> int\n    return fine\n");
}

#[test]
fn reserved_sudo_namespace_rejects_variants() {
    // Leading-underscore-stripped, case-folded `sudo_` is reserved
    // (spec/lockstep.md §7), regardless of spelling variant.
    let e = err("func sudo_foo() -> int\n    return 1\n");
    assert!(e.to_lowercase().contains("reserved"), "{e}");
    let e = err("func _Sudo_X() -> int\n    return 1\n");
    assert!(e.to_lowercase().contains("reserved"), "{e}");
    let e = err("func SUDO_x() -> int\n    return 1\n");
    assert!(e.to_lowercase().contains("reserved"), "{e}");
}

#[test]
fn reserved_sudo_namespace_covers_every_declaration_kind() {
    assert!(err("record sudo_r\n    x: int\nfunc f() -> int\n    return 1\n")
        .to_lowercase()
        .contains("reserved"));
    assert!(err("enum sudo_e\n    A\nfunc f() -> int\n    return 1\n")
        .to_lowercase()
        .contains("reserved"));
    assert!(err("enum E\n    sudo_v\nfunc f(e: E)\n    skip\n")
        .to_lowercase()
        .contains("reserved"));
    assert!(err("sudo_k = 1\nfunc f() -> int\n    return sudo_k\n")
        .to_lowercase()
        .contains("reserved"));
    assert!(err("func f(sudo_p: int)\n    skip\n").to_lowercase().contains("reserved"));
    assert!(err("func f()\n    sudo_l = 1\n").to_lowercase().contains("reserved"));
}

#[test]
fn sudo_lookalikes_without_the_reserved_prefix_stay_legal() {
    // `sudoku` and `sudo` (no underscore before a lowercase letter)
    // are NOT in the reserved namespace.
    let m = ok("func sudoku() -> int\n    return 1\nfunc sudo() -> int\n    return 2\n");
    assert!(m.func("sudoku").is_some());
    assert!(m.func("sudo").is_some());
}
