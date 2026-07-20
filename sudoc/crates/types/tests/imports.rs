//! Multi-module program checking: imports, qualified calls, cross-module
//! generic instantiation, and the v1 restriction that module-local types
//! do not cross boundaries.

use std::path::PathBuf;

fn program(name: &str, files: &[(&str, &str)]) -> Result<sudoc_types::Program, String> {
    let dir = std::env::temp_dir().join(format!("sudoc-imports-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (fname, src) in files {
        std::fs::write(dir.join(format!("{fname}.sudo")), src).unwrap();
    }
    let entry: PathBuf = dir.join(format!("{}.sudo", files.last().unwrap().0));
    sudoc_types::check_program(&entry).map_err(|es| es[0].msg.clone())
}

#[test]
fn qualified_calls_and_consts() {
    let util = "answer = 42\nfunc double(x: int) -> int\n    return x * 2\n";
    let main = "import util\n\nfunc go() -> int\n    return util.double(util.answer)\n\ntest \"works\"\n    assert go() == 84\n";
    let p = program("basic", &[("util", util), ("main", main)]).expect("checks");
    assert_eq!(p.modules.len(), 2);
    assert_eq!(p.modules[0].name, "util");
    assert_eq!(p.modules[1].name, "main");
    // The call is recorded qualified.
    let ir = sudoc_ir::pretty::dump(&p.modules[1]);
    assert!(ir.contains("util.double("), "{ir}");
    assert!(ir.contains("const:util.answer"), "{ir}");
}

#[test]
fn cross_module_generic_instantiates_in_defining_module() {
    let util = "func id<T>(x: T) -> T\n    return x\n";
    let main = "import util\n\nfunc go() -> int\n    return util.id(7)\n";
    let p = program("xgen", &[("util", util), ("main", main)]).expect("checks");
    let util_ir = &p.modules[0];
    assert!(util_ir.func("id__i64").is_some(), "{:?}",
        util_ir.funcs.iter().map(|f| f.name.clone()).collect::<Vec<_>>());
    let ir = sudoc_ir::pretty::dump(&p.modules[1]);
    assert!(ir.contains("util.id__i64("), "{ir}");
}

#[test]
fn transitive_imports_work() {
    let a = "func one() -> int\n    return 1\n";
    let b = "import a\n\nfunc two() -> int\n    return a.one() + 1\n";
    let c = "import b\n\nfunc three() -> int\n    return b.two() + 1\n\ntest \"t\"\n    assert three() == 3\n";
    let p = program("trans", &[("a", a), ("b", b), ("c", c)]).expect("checks");
    assert_eq!(p.modules.len(), 3);
}

#[test]
fn circular_imports_rejected() {
    let a = "import b\n\nfunc fa() -> int\n    return 1\n";
    let b = "import a\n\nfunc fb() -> int\n    return 1\n";
    let e = program("cycle", &[("a", a), ("b", b)]).unwrap_err();
    assert!(e.to_lowercase().contains("circular"), "{e}");
}

#[test]
fn missing_module_rejected() {
    let main = "import nowhere\n\nfunc f() -> int\n    return 1\n";
    let e = program("missing", &[("main", main)]).unwrap_err();
    assert!(e.contains("nowhere"), "{e}");
}

#[test]
fn module_local_types_do_not_cross() {
    let shapes = "record Point\n    x: int\n    y: int\nfunc origin() -> Point\n    return Point(0, 0)\n";
    let main = "import shapes\n\nfunc f() -> int\n    p = shapes.origin()\n    return 0\n";
    let e = program("localtypes", &[("shapes", shapes), ("main", main)]).unwrap_err();
    assert!(e.to_lowercase().contains("module"), "{e}");
}

#[test]
fn unknown_member_of_module() {
    let util = "func one() -> int\n    return 1\n";
    let main = "import util\n\nfunc f() -> int\n    return util.two()\n";
    let e = program("nomember", &[("util", util), ("main", main)]).unwrap_err();
    assert!(e.contains("two"), "{e}");
}
