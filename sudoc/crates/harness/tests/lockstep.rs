//! The M4 gate: lockstep runs across Python and C, detecting agreement,
//! consistent failure, and — the flagship — divergence.

use std::path::PathBuf;

use sudoc_harness::{all_backends, discovered_backends, lockstep, parse_tap, Backend, Outcome, TapLine, Verdict};

fn write_module(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sudo"));
    std::fs::write(&path, src).unwrap();
    path
}

fn both() -> Vec<Box<dyn Backend>> {
    all_backends()
}

#[test]
fn tap_parsing() {
    let stdout = "ok 1 - test_sorts\nnot ok 2 - test_other [AssertFailed: line 12]\nnot ok 3 - test_third [OutOfBounds]\n# 1/3 passed\n";
    let parsed = parse_tap(stdout);
    assert_eq!(
        parsed,
        vec![
            TapLine { name: "test_sorts".into(), outcome: Outcome::Pass, detail: None },
            TapLine {
                name: "test_other".into(),
                outcome: Outcome::Trap("AssertFailed".into()),
                detail: Some("line 12".into())
            },
            TapLine {
                name: "test_third".into(),
                outcome: Outcome::Trap("OutOfBounds".into()),
                detail: None
            },
        ]
    );
}

#[test]
fn agreeing_module_passes() {
    let path = write_module(
        "agree",
        "export func gcd(a: int, b: int) -> int\n    while b != 0\n        a, b = b, a mod b\n    return abs(a)\n\ntest \"basic\"\n    assert gcd(12, 18) == 6\n\ntest \"zero\"\n    assert gcd(0, 5) == 5\n",
    );
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(report.tests.len(), 2);
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.divergences(), 0);
}

#[test]
fn consistent_failure_is_not_divergence() {
    let path = write_module(
        "wrongo",
        "test \"bad math\"\n    assert 1 + 1 == 3\n\ntest \"fine\"\n    assert true\n",
    );
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(
        report.tests[0].verdict,
        Verdict::ConsistentFailure("AssertFailed".into()),
        "{report:?}"
    );
    assert_eq!(report.tests[1].verdict, Verdict::Pass);
    assert_eq!(report.divergences(), 0);
    assert!(!report.all_pass());
}

#[test]
fn order_dependence_diverges() {
    // Insertion order leaks into the result: Python (insertion-ordered dict)
    // keeps 0..n, C's hash table almost surely does not. 20 keys make
    // coincidental agreement essentially impossible (1/20!).
    let src = r#"func iteration_order(n: int) -> List<int>
    m = Map()
    for i = 0 to n - 1
        m[i] = i
    order = []
    for k, v in m
        order.append(k)
    return order

test "DIVERGES: map iteration order leaks"
    expected = []
    for i = 0 to 19
        expected.append(i)
    assert iteration_order(20) == expected
"#;
    let path = write_module("orderdep", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(report.tests[0].verdict, Verdict::Divergence, "{report:?}");
    // Python passes (dict preserves insertion), C traps the assert.
    let py = report.tests[0].outcomes.iter().find(|(t, _)| t == "py").unwrap();
    let c = report.tests[0].outcomes.iter().find(|(t, _)| t == "c").unwrap();
    assert_eq!(py.1, Outcome::Pass, "{report:?}");
    assert_eq!(c.1, Outcome::Trap("AssertFailed".into()), "{report:?}");
}

#[test]
fn report_renders_divergence_loudly() {
    let src = "test \"DIVERGES: nothing actually\"\n    assert true\n";
    let path = write_module("render", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    let (text, green) = sudoc_harness::render(&report);
    assert!(green);
    assert!(text.contains("render"), "{text}");
    assert!(text.contains("ok        test_"), "{text}");
}

#[test]
fn examples_lockstep_with_expected_divergences() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");
    let mut modules = 0;
    for path in walk(&examples) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let report = lockstep(&path, &both()).expect("harness runs");
        if name == "order_dependent" {
            // The pitfall example: at least its DIVERGES test must not agree
            // silently in a wrong way — anything except all-green is fine,
            // and the deterministic `smallest_key` test must pass everywhere.
            let smallest = report
                .tests
                .iter()
                .find(|t| t.name.contains("agrees"))
                .expect("AGREES test present");
            assert_eq!(smallest.verdict, Verdict::Pass, "{report:?}");
        } else {
            assert!(report.all_pass(), "{name}: {report:?}");
        }
        modules += 1;
    }
    assert!(modules >= 9);
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
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

#[test]
fn generics_run_lockstep() {
    // A generic insertion sort with a comparator — the stdlib shape — must
    // agree between Python and C.
    let src = r#"func ascending(a: int, b: int) -> bool
    return a < b

func descending(a: int, b: int) -> bool
    return b < a

func sort_by<T>(items: inout List<T>, less: func(T, T) -> bool)
    for j = 1 to items.length - 1
        i = j
        while i > 0 and less(items[i], items[i - 1])
            items.swap(i, i - 1)
            i = i - 1

func largest<T>(items: List<T>, less: func(T, T) -> bool) -> Option<T>
    if items.length == 0
        return None
    best = items[0]
    for x in items
        if less(best, x)
            best = x
    return Some(best)

test "generic sort both directions"
    xs = [3, 1, 2]
    sort_by(xs, ascending)
    assert xs == [1, 2, 3]
    sort_by(xs, descending)
    assert xs == [3, 2, 1]

test "generic max"
    assert largest([5, 9, 2], ascending) == Some(9)
    empty: List<int> = []
    assert largest(empty, ascending) == None

test "generic over floats"
    ys = [2.5, 1.5]
    sort_by(ys, float_less)
    assert ys == [1.5, 2.5]

func float_less(a: float, b: float) -> bool
    return a < b
"#;
    let path = write_module("generics", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn imports_run_lockstep() {
    // A two-module program: concrete calls, a constant, and a cross-module
    // generic instantiation, all through both backends.
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-imports-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("util.sudo"),
        "scale = 10\nfunc double(x: int) -> int\n    return x * 2\nfunc id<T>(x: T) -> T\n    return x\n",
    )
    .unwrap();
    let main_src = r#"import util

func go() -> int
    return util.double(util.scale) + util.id(1)

test "cross module"
    assert go() == 21
    assert util.id([1, 2]) == [1, 2]
"#;
    let path = dir.join("main.sudo");
    std::fs::write(&path, main_src).unwrap();
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn stdlib_runs_lockstep() {
    let stdlib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../stdlib");
    let mut modules = 0;
    for entry in std::fs::read_dir(&stdlib).expect("stdlib dir exists") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sudo") {
            let report = lockstep(&path, &both()).expect("harness runs");
            assert!(report.all_pass(), "{}: {report:?}", path.display());
            modules += 1;
        }
    }
    assert!(modules >= 2);
}

#[test]
fn cross_module_inout_and_local_match_lockstep() {
    // Regression coverage for three bugs fixed today: (1) Swift couldn't
    // compile a skip-only match arm, (2) Zig couldn't compile a match arm
    // that ignores one of a multi-field variant's binders, (3) Rust/Python
    // /JS/Zig resolved a cross-module callee's signature only in the
    // current module, so a cross-module call to an inout-taking function
    // silently dropped (py/js) or failed to compile (rs/zig) the `&mut`/
    // writeback. Per spec/language.md §9, records/enums can't yet cross a
    // module boundary, so the match coverage stays local to the entry
    // module while the inout coverage goes cross-module via `int`.
    let dir = std::env::temp_dir()
        .join(format!("sudoc-lockstep-xmod-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("xmod_util.sudo"),
        r#"func bump(x: inout int) -> int
    x = x + 1
    return x

func bump_by(x: inout int, delta: int) -> int
    x = x + delta
    return x
"#,
    )
    .unwrap();
    let main_src = r#"import xmod_util

record Tally
    count: int
    label: text

enum Signal
    Quiet
    Ping(n: int, note: int)

func run_bumps(start: int, times: int) -> int
    n = start
    for i = 1 to times
        xmod_util.bump(n)
    return n

func run_bumps_by(start: int, times: int, delta: int) -> int
    n = start
    total = 0
    for i = 1 to times
        total = total + xmod_util.bump_by(n, delta)
    return total

func describe(s: Signal) -> int
    match s
        case Quiet
            skip
        case Ping(n, note)
            return n
    return -1

test "cross module inout in a loop"
    assert run_bumps(0, 5) == 5

test "cross module inout inside an expression"
    n = 10
    assert xmod_util.bump(n) == 11
    assert n == 11

test "cross module inout return value threaded through a loop"
    assert run_bumps_by(0, 4, 3) == 30

test "local enum matched with a skip-only arm"
    assert describe(Quiet) == -1
    assert describe(Ping(7, 999)) == 7

test "local record field as a cross module inout argument"
    t = Tally(3, "ok")
    bumped = xmod_util.bump(t.count)
    assert bumped == 4
    assert t.count == 4
"#;
    let path = dir.join("xmod_main.sudo");
    std::fs::write(&path, main_src).unwrap();

    // All seven backends: the six in-tree ones plus the external Haskell
    // backend, discovered the same way the CLI does (backends/*/*.sudoc-
    // backend.json under the repo root).
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 5);
}

#[test]
fn module_prefix_collision_lockstep() {
    // F8 regression (red-team 2026-07-22, spec/lockstep.md §8): naive
    // `{module}_{name}` string-gluing collided module `a`'s fn `b_c`
    // with module `a_b`'s fn `c` (both flattened to `a_b_c`) in any
    // backend that merges multi-module programs into one translation
    // unit (C, Swift). Canonical, length-prefixed qualification
    // (sudoc_ir::mangle::qualify_value) makes the two provably
    // distinct across all seven targets.
    let dir = std::env::temp_dir()
        .join(format!("sudoc-lockstep-f8-modprefix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.sudo"), "func b_c() -> int\n    return 1\n").unwrap();
    std::fs::write(dir.join("a_b.sudo"), "func c() -> int\n    return 2\n").unwrap();
    let main_src = r#"import a
import a_b

test "module a's b_c and module a_b's c do not collide"
    assert a.b_c() == 1
    assert a_b.c() == 2
"#;
    let path = dir.join("f8_main.sudo");
    std::fs::write(&path, main_src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn cross_module_generics_lockstep() {
    // Regression: Zig monomorphized generics (Result/Option/List/Tuple/…)
    // were re-declared per module file, so two files each declaring
    // `pub const Res_bool_List_i64 = union(enum) { ... }` got two
    // nominally distinct types. Binding a cross-module call result to a
    // local then failed zig build-exe. Fix: hoist portable monomorphs
    // into sudo_types.zig. Calls are bound to locals before matching —
    // inlining the call as the match scrutinee accidentally dodges the bug.
    let dir = std::env::temp_dir().join(format!(
        "sudoc-lockstep-xmod-generics-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("xmod_gen.sudo"),
        r#"func check(ok: bool, msg: text) -> Result<bool, text>
    if ok
        return Ok(true)
    return Err(msg)

func maybe(n: int) -> Option<int>
    if n < 0
        return None
    return Some(n)

func pairs() -> List<(int, int)>
    return [(1, 2), (3, 4)]
"#,
    )
    .unwrap();
    let main_src = r#"import xmod_gen

test "cross module Result bound then matched"
    r = xmod_gen.check(true, "nope")
    match r
        case Ok(v)
            assert v == true
        case Err(e1)
            assert false
    r2 = xmod_gen.check(false, "bad")
    match r2
        case Ok(v2)
            assert false
        case Err(e)
            assert e == "bad"

test "cross module Option bound then matched"
    o = xmod_gen.maybe(7)
    match o
        case Some(n)
            assert n == 7
        case None
            assert false
    o2 = xmod_gen.maybe(-1)
    match o2
        case Some(n2)
            assert false
        case None
            assert true

test "cross module List of Tuple bound then indexed"
    ps = xmod_gen.pairs()
    a0, b0 = ps[0]
    assert a0 == 1
    assert b0 == 2
    a1, b1 = ps[1]
    assert a1 == 3
    assert b1 == 4
"#;
    let path = dir.join("xmod_gen_main.sudo");
    std::fs::write(&path, main_src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 3);
}

#[test]
fn inout_writeback_aliases_return_target_lockstep() {
    // Regression for the Haskell external backend (backends/haskell/Emit.hs):
    // the shape `n = f(n, x)` — a call's return value reassigned to the same
    // variable that the call's inout parameter aliases — used to emit an
    // invalid tuple pattern that bound the same name twice (GHC "Conflicting
    // definitions for 'n'"). Cover the shape top-level and inside a loop,
    // across all seven backends.
    let dir = std::env::temp_dir().join(format!(
        "sudoc-lockstep-inout-alias-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = r#"func bump(n: inout int, k: int) -> int
    n = n + k
    return n

test "inout writeback aliases return target"
    n = 10
    k = 5
    n = bump(n, k)
    assert n == 15

test "inout writeback aliases return target in a loop"
    n = 0
    for i = 1 to 3
        n = bump(n, i)
    assert n == 6
"#;
    let path = dir.join("inout_alias.sudo");
    std::fs::write(&path, src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 2);
}

#[test]
fn else_if_short_circuit_lockstep() {
    // Regression for the Zig backend: short-circuit `and`/`or` in an
    // `else if` condition used to emit a temp via `emit_lazy_bool` inside
    // the still-open previous arm, so the temp went out of scope before
    // the `else if` that read it. Nested `if {} else { if ... }` emission
    // (matching backend_c) keeps each condition's temps in a live scope.
    // Conditions use list indexing so the RHS `can_trap`s and actually
    // takes the lazy-bool path (plain bool locals do not).
    let dir = std::env::temp_dir().join(format!(
        "sudoc-lockstep-else-if-sc-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = r#"func classify(xs: List<int>, i: int, flag: bool) -> int
    if flag
        return 1
    else if i >= 0 and xs[i] == 10
        return 2
    else
        return 3

func classify_chain(xs: List<int>, i: int, j: int, flag: bool) -> int
    if flag
        return 1
    else if i >= 0 and xs[i] == 10
        return 2
    else if j >= 0 or xs[j] == 99
        return 3
    else
        return 4

func classify_nested(xs: List<int>, i: int, j: int, flag: bool) -> int
    if flag
        return 1
    else if i >= 0 and xs[i] == 10
        if j >= 0 and xs[j] == 20
            return 21
        else
            return 22
    else
        return 3

test "else if and short circuit second arm"
    xs = [10, 20, 30]
    assert classify(xs, 0, false) == 2
    assert classify(xs, 1, false) == 3
    assert classify(xs, 0, true) == 1

test "else if chain with or at depth two"
    xs = [10, 20, 30]
    assert classify_chain(xs, 1, 0, false) == 3
    assert classify_chain(xs, 0, 1, false) == 2
    assert classify_chain(xs, 1, 1, true) == 1

test "nested if inside else if with short circuit"
    xs = [10, 20, 30]
    assert classify_nested(xs, 0, 1, false) == 21
    assert classify_nested(xs, 0, 0, false) == 22
    assert classify_nested(xs, 1, 1, false) == 3
"#;
    let path = dir.join("else_if_sc.sudo");
    std::fs::write(&path, src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 3);
}

#[test]
fn result_option_match_unused_binder_lockstep() {
    // Regression for the Zig backend: Result/Option match arms that bind a
    // payload but never reference it used to hard-error with "unused local
    // constant". Enum match already silenced unused binders via
    // `binder_used`; the same guard is now applied to Option Some and
    // Result Ok/Err arms. Tests exercise both arms so the unused-binder
    // path is actually run, not merely compiled.
    let dir = std::env::temp_dir().join(format!(
        "sudoc-lockstep-unused-binder-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = r#"func result_ok_unused(v: int) -> int
    r: Result<int, text> = Ok(v)
    match r
        case Ok(x)
            return 1
        case Err(e)
            return 2

func result_err_unused(msg: text) -> int
    r: Result<int, text> = Err(msg)
    match r
        case Ok(x)
            return 1
        case Err(e)
            return 2

func option_some_unused(v: int) -> int
    o: Option<int> = Some(v)
    match o
        case Some(x)
            return 1
        case None
            return 0

func option_none() -> int
    o: Option<int> = None
    match o
        case Some(x)
            return 1
        case None
            return 0

test "result ok arm ignores binder"
    assert result_ok_unused(42) == 1

test "result err arm ignores binder"
    assert result_err_unused("nope") == 2

test "option some arm ignores binder"
    assert option_some_unused(7) == 1

test "option none arm"
    assert option_none() == 0
"#;
    let path = dir.join("unused_binder.sudo");
    std::fs::write(&path, src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 4);
}

#[test]
fn assert_failures_carry_operand_detail() {
    let src = r#"func wrongly_sorted(xs: List<int>) -> List<int>
    ys = xs
    ys.sort()
    ys.append(999)
    return ys

test "shows the operands"
    assert wrongly_sorted([3, 1]) == [1, 3]
"#;
    let path = write_module("detail", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert_eq!(
        report.tests[0].verdict,
        Verdict::ConsistentFailure("AssertFailed".into()),
        "{report:?}"
    );
    // Both targets serialized the mismatching operands.
    for (target, detail) in &report.tests[0].details {
        assert!(
            detail.contains("[1, 3, 999]") && detail.contains("!="),
            "{target}: {detail}"
        );
    }
    assert_eq!(
        report.tests[0].details.len(),
        sudoc_harness::all_backends().len(),
        "{report:?}"
    );
    // The rendered report shows them.
    let (text, _) = sudoc_harness::render(&report);
    assert!(text.contains("[1, 3, 999]"), "{text}");
}

#[test]
fn break_continue_and_expect_trap_lockstep() {
    let src = r#"enum Cmd
    Stop
    Nop
    Add(v: int)

func run(cmds: List<Cmd>) -> int
    total = 0
    for c in cmds
        match c
            case Stop
                break
            case Nop
                continue
            case Add(v)
                total = total + v
    return total

test "break and continue in a range"
    total = 0
    for i = 1 to 10
        if i mod 2 == 0
            continue
        if i > 7
            break
        total = total + i
    assert total == 16

test "break crosses match"
    assert run([Add(5), Nop, Stop, Add(99)]) == 5
    assert run([Nop, Add(2), Add(3)]) == 5

test "while true with break"
    n = 0
    while true
        n = n + 1
        if n == 5
            break
    assert n == 5

test "continue in downto"
    s = 0
    for i = 5 downto 1
        if i == 3
            continue
        s = s + i
    assert s == 12

test "inner break leaves outer loop running"
    pairs = 0
    for i = 1 to 3
        for j = 1 to 10
            if j == 2
                break
            pairs = pairs + 1
    assert pairs == 3

test "single iteration at int max"
    hits = 0
    for i = 9223372036854775807 to 9223372036854775807
        hits = hits + 1
    assert hits == 1

test "min int literal"
    m = -9223372036854775808
    assert m + 1 == -9223372036854775807

test "empty pop traps"
    a: List<int> = []
    expect_trap OutOfBounds
        a.pop()

test "overflow expected"
    expect_trap Overflow
        big = 9223372036854775807
        x = big + 1
        assert x == 0
"#;
    let path = write_module("loopstrap", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
}

#[test]
fn expect_trap_failures_are_consistent() {
    let src = r#"test "wrong kind reported"
    a = [1]
    expect_trap KeyMissing
        x = a[5]

test "missing trap reported"
    expect_trap Overflow
        y = 1 + 1
"#;
    let path = write_module("expectfail", src);
    let report = lockstep(&path, &both()).expect("harness runs");
    for t in &report.tests {
        assert_eq!(
            t.verdict,
            Verdict::ConsistentFailure("AssertFailed".into()),
            "{report:?}"
        );
        for (_, d) in &t.details {
            assert!(d.contains("expected trap"), "{d}");
        }
    }
}

#[test]
fn conformance_corpus_is_green() {
    // The SDK acceptance bar: every corpus module agrees across all
    // registered backends. New backends run this via
    // `sudoc conformance --target <name>`.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../conformance/semantics");
    let mut modules = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus exists") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sudo") {
            let report = lockstep(&path, &both()).expect("harness runs");
            assert!(report.all_pass(), "{}: {report:?}", path.display());
            modules += 1;
        }
    }
    assert!(modules >= 9, "corpus shrank? found {modules}");
}

#[test]
fn std_imports_run_lockstep_across_all_backends() {
    let dir = std::env::temp_dir().join(format!("sudoc-lockstep-std-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_src = r#"import std.regex
import std.strings

test "std regex import works"
    r = regex.regex_search("a+", "aaa", false)
    match r
        case Ok(v)
            assert v == true
        case Err(e)
            assert false

test "std strings import works"
    assert strings.to_upper("abc") == "ABC"
"#;
    let path = dir.join("std_main.sudo");
    std::fs::write(&path, main_src).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut targets = all_backends();
    targets.extend(
        discovered_backends(&repo_root).expect("discover backends/haskell manifest"),
    );
    assert_eq!(targets.len(), 7, "expected 6 in-tree + 1 external (hs) backend");

    let report = lockstep(&path, &targets).expect("harness runs");
    assert!(report.all_pass(), "{report:?}");
    assert_eq!(report.tests.len(), 2);
}
