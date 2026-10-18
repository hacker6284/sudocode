//! Behavioral checks for inlining, number lowering, and in-place list updates.
//! These run the emitted JavaScript. They fail if a shared list is mutated,
//! if an inlined add above 2^53 loses precision, or if a uniquely owned list
//! rebuild allocates a fresh list.

use std::process::{Command, Output};

fn run(name: &str, sudo_src: &str, driver: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("sudoc-jslower-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(sudo_src, name).expect("checks");
    std::fs::write(
        dir.join(sudoc_backend_js::impl_file(name)),
        sudoc_backend_js::emit(&ir, true),
    )
    .unwrap();
    std::fs::write(
        dir.join(sudoc_backend_js::RUNTIME_FILE),
        sudoc_backend_js::RUNTIME,
    )
    .unwrap();
    std::fs::write(dir.join("driver.mjs"), driver).unwrap();
    let tests = Command::new("node")
        .current_dir(&dir)
        .arg(sudoc_backend_js::impl_file(name))
        .output()
        .expect("node runs tests");
    let test_out = String::from_utf8_lossy(&tests.stdout);
    if !tests.status.success() || !test_out.contains("3/3 passed") {
        eprintln!(
            "tests stdout:\n{test_out}\nstderr:\n{}",
            String::from_utf8_lossy(&tests.stderr)
        );
        std::fs::remove_dir_all(&dir).ok();
        return tests;
    }
    let out = Command::new("node")
        .current_dir(&dir)
        .arg("driver.mjs")
        .output()
        .expect("node runs");
    if !out.status.success() {
        eprintln!(
            "impl:\n{}",
            std::fs::read_to_string(dir.join(sudoc_backend_js::impl_file(name)))
                .unwrap_or_default()
        );
    }
    std::fs::remove_dir_all(&dir).ok();
    out
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("ALL OK"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

const SRC: &str = r#"
func add(a: int, b: int) -> int
    return a + b

func big(a: int, b: int) -> int
    return add(a, b)

func bump(xs: List<int>) -> List<int>
    out: List<int> = []
    for i = 0 to xs.length - 1
        out.append(xs[i] + 1)
    return out

func pipe() -> List<int>
    xs = [1, 2, 3]
    xs = bump(xs)
    return xs

record P
    x: int

func setx(p: P, v: int) -> P
    return P(v)

func map_set(xs: List<P>) -> List<P>
    out: List<P> = []
    for i = 0 to xs.length - 1
        out.append(setx(xs[i], xs[i].x + 1))
    return out

func widen(xs: List<int>) -> List<int>
    out: List<int> = []
    for i = 0 to xs.length - 1
        out.append(xs[i] + 9007199254740992)
    return out

test "inlined add above 2^53 stays exact"
    assert big(9007199254740992, 3) == 9007199254740995
    assert widen([3]) == [9007199254740995]

test "shared list is not mutated"
    a = [1, 2, 3]
    b = a
    c = bump(a)
    assert a == [1, 2, 3]
    assert b == a
    assert c == [2, 3, 4]

test "unique rebuild is the incremented list"
    assert pipe() == [2, 3, 4]
"#;

#[test]
fn overflow_fallback_and_aliasing() {
    let driver = r#"
import assert from "node:assert/strict";
import * as impl from "./_lower_impl.mjs";
import * as rt from "./_sudo_rt.mjs";

const sum = impl.big(9007199254740992n, 3n);
assert.equal(sum, 9007199254740995n);

const shared = rt.lst([1n, 2n, 3n]);
const alias = shared.share();
const bumped = impl.bump(shared);
assert.equal(rt.eq(alias, rt.lst([1n, 2n, 3n])), true);
assert.equal(rt.eq(bumped, rt.lst([2n, 3n, 4n])), true);
assert.notEqual(bumped._box, alias._box);

const unique = rt.lst([1n, 2n, 3n]);
const same = impl.bump(unique);
assert.equal(same._box, unique._box);
assert.equal(typeof same._box.d[0], "number");
assert.equal(same._box.d[0], 2);
assert.equal(rt.eq(same, rt.lst([2n, 3n, 4n])), true);

const wideIn = rt.lst([3n]);
const wide = impl.widen(wideIn);
assert.equal(wide._box, wideIn._box);
assert.equal(rt.eq(wide, rt.lst([9007199254740995n])), true);

const p = new impl.P(1n);
const ps = rt.lst([p]);
const mapped = impl.map_set(ps);
assert.equal(mapped._box, ps._box);
assert.equal(p.x === 1n || p.x === 1, true);
const got = mapped._box.d[0].x;
assert.equal(got === 2 || got === 2n, true);
assert.notEqual(mapped._box.d[0], p);

console.log("ALL OK");
"#;
    assert_ok(&run("lower", SRC, driver));
}
