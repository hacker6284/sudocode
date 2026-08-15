//! v0.7: program-global nominal identity, `sudo_types` placement, and the
//! negative matrix pinned to exact messages.

use sudoc_ir::mangle::SHARED_TYPES_MODULE;

fn program(_name: &str, files: &[(&str, &str)]) -> Result<sudoc_types::Program, String> {
    sudoc_types::check_program_files(files).map_err(|es| es[0].msg.clone())
}

fn names(p: &sudoc_types::Program) -> Vec<String> {
    p.modules.iter().map(|m| m.name.clone()).collect()
}

fn module<'a>(p: &'a sudoc_types::Program, name: &str) -> &'a sudoc_ir::IrModule {
    p.modules
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("missing module {name} in {:?}", names(p)))
}

// ---- IR shape ----------------------------------------------------------------

#[test]
fn nothing_escaping_emits_no_sudo_types() {
    let util = "record Local\n    n: int\nfunc one() -> int\n    x = Local(1)\n    return x.n\n";
    let main = "import util\n\nrecord Home\n    n: int\n\nfunc go() -> int\n    return util.one()\n\ntest \"t\"\n    assert go() == 1\n";
    let p = program("noescape", &[("util", util), ("main", main)]).expect("checks");
    assert!(
        !names(&p).contains(&SHARED_TYPES_MODULE.to_string()),
        "{:?}",
        names(&p)
    );
    assert_eq!(names(&p), vec!["util".to_string(), "main".to_string()]);
    assert!(module(&p, "util").record("Local").is_some());
    assert!(module(&p, "main").record("Home").is_some());
    assert!(module(&p, "util")
        .imports
        .iter()
        .all(|i| i != SHARED_TYPES_MODULE));
    assert!(module(&p, "main")
        .imports
        .iter()
        .all(|i| i != SHARED_TYPES_MODULE));
}

#[test]
fn escaping_type_moves_to_sudo_types_first() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = util.id(Thing(7))\n    return t.n\n";
    let p = program("escapes", &[("util", util), ("main", main)]).expect("checks");
    assert_eq!(p.modules[0].name, SHARED_TYPES_MODULE);
    assert!(p.modules[0].imports.is_empty());
    assert!(p.modules[0].funcs.is_empty());
    assert!(p.modules[0].consts.is_empty());
    assert!(p.modules[0].tests.is_empty());
    assert!(module(&p, SHARED_TYPES_MODULE).record("Thing").is_some());
    assert!(module(&p, "main").record("Thing").is_none());
    assert_eq!(module(&p, "util").imports[0], SHARED_TYPES_MODULE);
    assert_eq!(module(&p, "main").imports[0], SHARED_TYPES_MODULE);
}

#[test]
fn instantiations_land_in_the_defining_module() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = util.id(Thing(1))\n    return t.n\n";
    let p = program("instplace", &[("util", util), ("main", main)]).expect("checks");
    let util_ir = module(&p, "util");
    assert!(
        util_ir
            .funcs
            .iter()
            .any(|f| f.name.starts_with("sudo_") && f.name.contains("id")),
        "{:?}",
        util_ir
            .funcs
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
    );
    assert!(module(&p, "main")
        .funcs
        .iter()
        .all(|f| !f.name.contains("id") || f.name == "go"));
}

#[test]
fn two_callers_same_type_one_instantiation() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = util.id(Thing(1))\n    u = util.id(Thing(2))\n    return t.n + u.n\n";
    let p = program("onesym", &[("util", util), ("main", main)]).expect("checks");
    let ids: Vec<_> = module(&p, "util")
        .funcs
        .iter()
        .filter(|f| f.name.contains("id"))
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
}

#[test]
fn two_callers_different_types_two_instantiations() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord A\n    n: int\nrecord B\n    n: int\n\nfunc go() -> int\n    x = util.id(A(1))\n    y = util.id(B(2))\n    return x.n + y.n\n";
    let p = program("twosym", &[("util", util), ("main", main)]).expect("checks");
    let ids: Vec<_> = module(&p, "util")
        .funcs
        .iter()
        .filter(|f| f.name.contains("id"))
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
}

#[test]
fn concrete_call_still_rejected_when_names_collide() {
    let left = "record Thing\n    n: int\nfunc mk() -> Thing\n    return Thing(1)\nfunc id<T>(x: T) -> T\n    return x\n";
    let right = "record Thing\n    n: int\nfunc mk() -> Thing\n    return Thing(2)\n";
    let main = "import left\nimport right\n\nfunc go() -> int\n    a = left.id(left.mk())\n    b = left.id(right.mk())\n    return a.n + b.n\n";
    let e = program(
        "collide_concrete",
        &[("left", left), ("right", right), ("main", main)],
    )
    .unwrap_err();
    assert_eq!(
        e,
        "'left.mk' uses module-local types in its signature and cannot be called across modules yet"
    );
}

#[test]
fn colliding_names_via_generics_both_qualified() {
    let idmod = "func id<T>(x: T) -> T\n    return x\n";
    let left = "import idmod\n\nrecord Thing\n    n: int\n\nfunc send() -> int\n    t = idmod.id(Thing(1))\n    return t.n\n";
    let right = "import idmod\n\nrecord Thing\n    n: int\n\nfunc send() -> int\n    t = idmod.id(Thing(2))\n    return t.n\n";
    let main =
        "import left\nimport right\n\nfunc go() -> int\n    return left.send() + right.send()\n";
    let p = program(
        "collide_gen",
        &[
            ("idmod", idmod),
            ("left", left),
            ("right", right),
            ("main", main),
        ],
    )
    .expect("checks");
    let st = module(&p, SHARED_TYPES_MODULE);
    assert_eq!(
        st.records.len(),
        2,
        "{:?}",
        st.records
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        st.records.iter().all(|r| r.name.starts_with("Sudo_M")),
        "{:?}",
        st.records
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    );
    assert_ne!(st.records[0].name, st.records[1].name);
}

#[test]
fn same_name_one_escaping_both_symbols_qualified() {
    let idmod = "func id<T>(x: T) -> T\n    return x\n";
    let other = "record Thing\n    n: int\nfunc local() -> int\n    return Thing(9).n\n";
    let main = "import idmod\nimport other\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = idmod.id(Thing(1))\n    return t.n + other.local()\n";
    let p = program(
        "oneesc",
        &[("idmod", idmod), ("other", other), ("main", main)],
    )
    .expect("checks");
    // Both Things get qualified IR symbols (name is not unique program-wide).
    let st = module(&p, SHARED_TYPES_MODULE);
    assert_eq!(st.records.len(), 1);
    assert!(
        st.records[0].name.starts_with("Sudo_M"),
        "{}",
        st.records[0].name
    );
    let other_recs: Vec<_> = module(&p, "other")
        .records
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(other_recs.len(), 1);
    assert!(other_recs[0].starts_with("Sudo_M"), "{other_recs:?}");
    assert_ne!(st.records[0].name, other_recs[0]);
}

#[test]
fn closure_moves_field_type_too() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Inner\n    n: int\nrecord Outer\n    inner: Inner\n\nfunc go() -> int\n    t = util.id(Outer(Inner(3)))\n    return t.inner.n\n";
    let p = program("closure", &[("util", util), ("main", main)]).expect("checks");
    let st = module(&p, SHARED_TYPES_MODULE);
    let recs: Vec<_> = st.records.iter().map(|r| r.name.clone()).collect();
    assert!(recs.iter().any(|n| n == "Inner"), "{recs:?}");
    assert!(recs.iter().any(|n| n == "Outer"), "{recs:?}");
}

#[test]
fn sort_by_thing_regression_checks() {
    let main = "import std.sorting\n\nrecord Thing\n    n: int\n\nfunc less(a: Thing, b: Thing) -> bool\n    return a.n < b.n\n\nfunc go() -> int\n    xs = [Thing(3), Thing(1), Thing(2)]\n    sorting.sort_by(xs, less)\n    return xs[0].n + xs[1].n * 10 + xs[2].n * 100\n";
    let p = program("sortby", &[("main", main)]).expect("checks");
    assert_eq!(p.modules[0].name, SHARED_TYPES_MODULE);
    assert!(module(&p, SHARED_TYPES_MODULE).record("Thing").is_some());
    assert!(module(&p, "sorting")
        .funcs
        .iter()
        .any(|f| f.name.contains("sort_by")));
}

// ---- negatives, exact messages ----------------------------------------------

#[test]
fn concrete_cross_module_nominal_signature() {
    let shapes =
        "record Point\n    x: int\n    y: int\nfunc origin() -> Point\n    return Point(0, 0)\n";
    let main = "import shapes\n\nfunc f() -> int\n    p = shapes.origin()\n    return 0\n";
    let e = program("concrete", &[("shapes", shapes), ("main", main)]).unwrap_err();
    assert_eq!(
        e,
        "'shapes.origin' uses module-local types in its signature and cannot be called across modules yet"
    );
}

#[test]
fn qualified_type_annotation_rejected() {
    let shapes = "record Point\n    x: int\n    y: int\nfunc origin() -> int\n    return 0\n";
    let main =
        "import shapes\n\nfunc f() -> int\n    p: shapes.Point = shapes.origin()\n    return 0\n";
    let e = program("qualtype", &[("shapes", shapes), ("main", main)]).unwrap_err();
    assert_eq!(
        e,
        "unknown type 'shapes.Point' (module-qualified types are not part of v1 — module-local records/enums cannot cross module boundaries, spec §9)"
    );
}

#[test]
fn stdlib_nominal_in_type_position_rejected() {
    let main = "import std.bigint\n\nfunc f() -> int\n    x: bigint.BigInt = bigint.big_from_int(1)\n    return 0\n";
    let e = program("stdnom", &[("main", main)]).unwrap_err();
    assert_eq!(
        e,
        "unknown type 'bigint.BigInt' (module-qualified types are not part of v1 — module-local records/enums cannot cross module boundaries, spec §9)"
    );
}

#[test]
fn reserved_module_name_sudo_types() {
    let dir = std::env::temp_dir().join(format!("sudoc-share-reserved-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sudo_types.sudo"),
        "func f() -> int\n    return 1\n",
    )
    .unwrap();
    let e = sudoc_types::check_program(&dir.join("sudo_types.sudo")).unwrap_err()[0]
        .msg
        .clone();
    assert_eq!(e, "module name 'sudo_types' is reserved for the compiler");
}

#[test]
fn import_reserved_module_name_rejected() {
    let main = "import sudo_types\n\nfunc f() -> int\n    return 1\n";
    let e = program("impreserved", &[("main", main)]).unwrap_err();
    assert_eq!(e, "module name 'sudo_types' is reserved for the compiler");
}

#[test]
fn type_arg_depth_guard() {
    let mut src = String::from(
        "func wrap<T>(x: T) -> List<T>\n    return [x]\n\nfunc go() -> int\n    a0 = 1\n",
    );
    for i in 0..33 {
        src.push_str(&format!("    a{} = wrap(a{i})\n", i + 1));
    }
    src.push_str("    return 0\n");
    let e = match sudoc_types::check_source(&src, "m") {
        Ok(_) => panic!("expected growth guard"),
        Err(es) => es[0].msg.clone(),
    };
    assert_eq!(
        e,
        "'wrap' instantiated at a type argument of depth 33 (limit 32)"
    );
}

#[test]
fn instantiation_count_guard() {
    let mut src = String::from("func id<T>(x: T) -> T\n    return x\n\n");
    for i in 0..257 {
        src.push_str(&format!("record R{i}\n    n: int\n"));
    }
    src.push_str("func go() -> int\n");
    for i in 0..257 {
        src.push_str(&format!("    x{i} = id(R{i}({i}))\n"));
    }
    src.push_str("    return 0\n");
    let e = match sudoc_types::check_source(&src, "m") {
        Ok(_) => panic!("expected count guard"),
        Err(es) => es[0].msg.clone(),
    };
    assert_eq!(e, "'id' instantiated more than 256 times");
}

#[test]
fn export_boundary_uses_resolved_ir_symbol() {
    let idmod = "func id<T>(x: T) -> T\n    return x\n";
    let other = "record Thing\n    n: int\nfunc local() -> int\n    return Thing(9).n\n";
    let main = "import idmod\nimport other\n\nrecord Thing\n    n: int\n\nexport func echo(t: Thing) -> Thing\n    return idmod.id(t)\n";
    let p = program(
        "boundcollide",
        &[("idmod", idmod), ("other", other), ("main", main)],
    )
    .expect("checks");
    let echo = module(&p, "main")
        .funcs
        .iter()
        .find(|f| f.name == "echo")
        .expect("echo");
    match &echo.params[0].boundary {
        sudoc_ir::BoundaryTy::Named(n) => {
            assert!(n.starts_with("Sudo_M"), "expected IR symbol, got {n}");
        }
        other => panic!("expected Named boundary, got {other:?}"),
    }
    match &echo.ret_boundary {
        Some(sudoc_ir::BoundaryTy::Named(n)) => {
            assert!(n.starts_with("Sudo_M"), "expected IR symbol, got {n}");
        }
        other => panic!("expected Named ret boundary, got {other:?}"),
    }
}

#[test]
fn instantiated_param_boundary_is_resolved_ty() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = util.id(Thing(1))\n    return t.n\n";
    let p = program("boundary", &[("util", util), ("main", main)]).expect("checks");
    let inst = module(&p, "util")
        .funcs
        .iter()
        .find(|f| f.name.starts_with("sudo_"))
        .expect("instantiation");
    assert_eq!(
        inst.params[0].boundary,
        sudoc_ir::BoundaryTy::Named("Thing".into())
    );
}

#[test]
fn instantiated_text_sibling_keeps_text_boundary() {
    let util = "func wrap<T>(s: text, x: T) -> T\n    return x\n";
    let main = "import util\n\nrecord Thing\n    n: int\n\nfunc go() -> int\n    t = util.wrap(\"a\", Thing(1))\n    return t.n\n";
    let p = program("textbound", &[("util", util), ("main", main)]).expect("checks");
    let inst = module(&p, "util")
        .funcs
        .iter()
        .find(|f| f.name.starts_with("sudo_"))
        .expect("instantiation");
    assert_eq!(inst.params[0].boundary, sudoc_ir::BoundaryTy::Text);
    assert_eq!(
        inst.params[1].boundary,
        sudoc_ir::BoundaryTy::Named("Thing".into())
    );
}

#[test]
fn diamond_without_generics_module_order() {
    let a = "func one() -> int\n    return 1\n";
    let b = "import a\n\nfunc two() -> int\n    return a.one() + 1\n";
    let c = "import a\n\nfunc three() -> int\n    return a.one() + 2\n";
    let main = "import b\nimport c\n\nfunc go() -> int\n    return b.two() + c.three()\n";
    let p = program("diamond", &[("a", a), ("b", b), ("c", c), ("main", main)]).expect("checks");
    assert!(
        !names(&p).contains(&SHARED_TYPES_MODULE.to_string()),
        "{:?}",
        names(&p)
    );
    assert_eq!(p.modules.last().unwrap().name, "main");
}
