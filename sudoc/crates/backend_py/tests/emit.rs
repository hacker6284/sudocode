use sudoc_backend_py::emit;
use sudoc_sdk::Backend;

fn py(src: &str) -> String {
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    emit(&ir, true)
}

#[test]
fn gcd_reads_like_python() {
    let out = py("export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n");
    assert!(out.contains("def gcd(a, b):"), "{out}");
    assert!(out.contains("while b != 0:"), "{out}");
    // mod is floor modulo with a zero-divisor trap — via the runtime.
    assert!(out.contains("_rt.mod_i64(a, b)"), "{out}");
    // abs on int wraps at i64 min — via the runtime.
    assert!(out.contains("_rt.abs_i64(a)"), "{out}");
}

#[test]
fn int_arithmetic_is_checked() {
    let out = py("func f(a: int, b: int) -> int\n    return a * b + 1\n");
    assert!(out.contains("_rt.chk("), "{out}");
}

#[test]
fn float_arithmetic_is_plain() {
    let out = py("func f(a: float, b: float) -> float\n    return a * b + 1.0\n");
    assert!(!out.contains("_rt.chk("), "{out}");
    assert!(out.contains("a * b + 1.0"), "{out}");
}

#[test]
fn single_inout_writeback() {
    let out = py("func bump(x: inout int)\n    x = x + 1\nfunc f() -> int\n    n = 0\n    bump(n)\n    return n\n");
    // Callee returns the inout value; call site reassigns it.
    assert!(out.contains("return x"), "{out}");
    assert!(out.contains("n = bump(n)"), "{out}");
}

#[test]
fn ret_plus_inout_unpacks() {
    let out = py("func take(x: inout int) -> int\n    x = x + 1\n    return x * 2\nfunc f() -> int\n    n = 0\n    y = take(n)\n    return y + n\n");
    assert!(out.contains("y, n = take(n)"), "{out}");
}

#[test]
fn composite_params_are_copied_on_entry() {
    // Body never writes `items` → entry-dup elided (never_written).
    let out = py("func f(items: List<int>) -> int\n    return items.length\n");
    assert!(!out.contains("items = _rt.dup(items)"), "{out}");
    // Body mutates `items` → entry-dup still required.
    let out_w = py("func h(items: List<int>)\n    items.append(1)\n");
    assert!(out_w.contains("items = _rt.dup(items)"), "{out_w}");
    // ...but inout params are not.
    let out2 = py("func g(items: inout List<int>)\n    items.append(1)\n");
    assert!(!out2.contains("_rt.dup(items)"), "{out2}");
    assert!(out2.contains("items.append(1)"), "{out2}");
}

#[test]
fn aliasing_assignment_copies() {
    let out = py("func f(a: List<int>) -> List<int>\n    b = a\n    b.append(1)\n    return b\n");
    assert!(out.contains("b = _rt.dup(a)"), "{out}");
    // Scalars never copy.
    let out2 = py("func g(x: int) -> int\n    y = x\n    return y\n");
    assert!(!out2.contains("dup"), "{out2}");
}

#[test]
fn deep_equality_via_runtime() {
    let out = py("func f(a: List<int>, b: List<int>) -> bool\n    return a == b\n");
    assert!(out.contains("_rt.eq(a, b)"), "{out}");
    let out2 = py("func g(x: int, y: int) -> bool\n    return x == y\n");
    assert!(out2.contains("x == y"), "{out2}");
}

#[test]
fn enums_become_dataclasses_and_match() {
    let out = py("enum Tree\n    Leaf\n    Node(value: int, left: Tree, right: Tree)\nfunc sum(t: Tree) -> int\n    match t\n        case Leaf\n            return 0\n        case Node(v, l, r)\n            return v + sum(l) + sum(r)\n");
    assert!(out.contains("class Sudo_4Tree_4Leaf"), "{out}");
    assert!(out.contains("class Sudo_4Tree_4Node"), "{out}");
    assert!(out.contains("match t:"), "{out}");
    assert!(out.contains("case Sudo_4Tree_4Node(v, l, r):"), "{out}");
    assert!(out.contains("case Sudo_4Tree_4Leaf():"), "{out}");
}

#[test]
fn option_uses_runtime_types() {
    let out = py("func f(o: Option<int>) -> int\n    match o\n        case Some(v)\n            return v\n        case None\n            return 0\n");
    assert!(out.contains("case _rt.Some(v):"), "{out}");
    assert!(out.contains("case _rt.NoneOpt():"), "{out}");
}

#[test]
fn tests_emit_with_runner() {
    let out = py("func double(x: int) -> int\n    return x * 2\ntest \"doubles work\"\n    assert double(2) == 4\n");
    assert!(out.contains("def test_doubles_work():"), "{out}");
    assert!(out.contains("_rt.run_tests"), "{out}");
    assert!(out.contains("_rt.sudo_assert_eq("), "{out}");
}

#[test]
fn library_mode_omits_tests() {
    let src =
        "func double(x: int) -> int\n    return x * 2\ntest \"t\"\n    assert double(2) == 4\n";
    let ir = sudoc_types::check_source(src, "m").expect("checks");
    let out = emit(&ir, false);
    assert!(!out.contains("def test_"), "{out}");
    assert!(!out.contains("run_tests"), "{out}");
}

#[test]
fn text_literals_render_readably() {
    let out = py("func f() -> text\n    return \"abc\"\n");
    assert!(out.contains("_rt.text(\"abc\")"), "{out}");
}

fn emit_program(name: &str, files: &[(&str, &str)]) -> Vec<sudoc_sdk::GeneratedFile> {
    let dir = std::env::temp_dir().join(format!("sudoc-py-share-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (fname, src) in files {
        std::fs::write(dir.join(format!("{fname}.sudo")), src).unwrap();
    }
    let entry = dir.join(format!("{}.sudo", files.last().unwrap().0));
    let p = sudoc_types::check_program(&entry).expect("checks");
    sudoc_backend_py::PythonBackend
        .emit_program(&p.modules, true)
        .expect("emit")
}

#[test]
fn sudo_types_file_absent_when_nothing_escapes() {
    let files = emit_program(
        "noesc",
        &[
            ("util", "func one() -> int\n    return 1\n"),
            (
                "main",
                "import util\n\nrecord Home\n    n: int\ntest \"t\"\n    assert util.one() == 1\n",
            ),
        ],
    );
    assert!(
        files.iter().all(|f| !f.path.contains("sudo_types")),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn sudo_types_file_present_when_type_crosses() {
    let files = emit_program(
        "esc",
        &[
            ("util", "func id<T>(x: T) -> T\n    return x\n"),
            (
                "main",
                "import util\n\nrecord Thing\n    n: int\ntest \"t\"\n    t = util.id(Thing(1))\n    assert t.n == 1\n",
            ),
        ],
    );
    assert!(
        files.iter().any(|f| f.path == "_sudo_types_impl.py"),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
    let util = files
        .iter()
        .find(|f| f.path == "_util_impl.py")
        .expect("util");
    assert!(
        util.contents
            .contains("import _sudo_types_impl as sudo_types"),
        "{}",
        util.contents
    );
    let types = files
        .iter()
        .find(|f| f.path == "_sudo_types_impl.py")
        .expect("sudo_types");
    assert!(types.contents.contains("class Thing"), "{}", types.contents);
    let main = files
        .iter()
        .find(|f| f.path == "_main_impl.py")
        .expect("main");
    assert!(
        main.contents.contains("sudo_types.Thing("),
        "{}",
        main.contents
    );
}

#[test]
fn escaped_inout_export_writeback_finds_record() {
    let files = emit_program(
        "expwb",
        &[
            ("util", "func id<T>(x: T) -> T\n    return x\n"),
            (
                "main",
                "import util\n\nrecord Thing\n    n: int\n\nexport func bump(t: inout Thing)\n    t.n = t.n + 1\n    x = util.id(t)\n    t.n = x.n\n",
            ),
        ],
    );
    let api = files.iter().find(|f| f.path == "main.py").expect("api");
    assert!(api.contents.contains("t.n = _new_t.n"), "{}", api.contents);
}
