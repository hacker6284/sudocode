//! Host-boundary adapter integration tests (lockstep.md §5.4).
//!
//! Each test generates an impl module + host-facing API adapter, then runs a
//! hand-written Node driver that imports the *adapter* (not the impl) and
//! asserts host-facing shapes, validation errors, defensive copies, and
//! writeback. Mirrors backend_py/tests/execute.rs::host_adapters_speak_python
//! but covers the full JS boundary table.

use std::process::{Command, Output};

/// Compile `sudo_src` as module `name`, write impl + api + runtime + driver,
/// run `node driver.mjs` with cwd = temp dir (relative entry path — see
/// execute.rs for the macOS /var vs /private/var reason).
fn run_driver(name: &str, sudo_src: &str, driver_js: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("sudoc-jsboundary-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ir = sudoc_types::check_source(sudo_src, name).expect("checks");
    std::fs::write(
        dir.join(sudoc_backend_js::impl_file(name)),
        sudoc_backend_js::emit(&ir, false),
    )
    .unwrap();
    let api = sudoc_backend_js::emit_api(&ir).expect("has an adaptable export");
    std::fs::write(dir.join(sudoc_backend_js::api_file(name)), api).unwrap();
    std::fs::write(dir.join(sudoc_backend_js::RUNTIME_FILE), sudoc_backend_js::RUNTIME).unwrap();
    std::fs::write(dir.join("driver.mjs"), driver_js).unwrap();
    let out = Command::new("node")
        .current_dir(&dir)
        .arg("driver.mjs")
        .output()
        .expect("node runs");
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

// ---- 1–4: int in/out, RangeError, bigint in, unsafe-out --------------------

#[test]
fn int_safe_range_number_in_out() {
    // Bullet 1: safe-range number in/out is a JS number with the right value.
    let src = r#"export func inc(x: int) -> int
    return x + 1
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./int_safe.mjs";

const result = lib.inc(41);
assert.equal(typeof result, "number");
assert.equal(result, 42);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("int_safe", src, driver));
}

#[test]
fn int_unsafe_number_rejects_with_range_error() {
    // Bullet 2: integer number outside ±(2^53−1) rejects on the way in.
    let src = r#"export func inc(x: int) -> int
    return x + 1
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./int_unsafe_in.mjs";

try {
    lib.inc(Number.MAX_SAFE_INTEGER + 10);
    console.log("FAIL: did not throw");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof RangeError, String(e));
}

console.log("ALL OK");
"#;
    assert_ok(&run_driver("int_unsafe_in", src, driver));
}

#[test]
fn int_bigint_in_works() {
    // Bullet 3: bigint is accepted on the way in, including values outside the
    // safe *number* range. Return a bool so the large value never needs int_out.
    let src = r#"export func is_nonzero(x: int) -> bool
    return x != 0
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./int_bigint_in.mjs";

// Outside Number.isSafeInteger range, but fine as bigint / i64.
assert.equal(lib.is_nonzero(9007199254740993n), true);
assert.equal(lib.is_nonzero(0n), false);
// Small bigint also works.
assert.equal(lib.is_nonzero(5n), true);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("int_bigint_in", src, driver));
}

#[test]
fn int_out_of_safe_range_result_throws_range_error() {
    // Bullet 4: internal BigInt result outside ±(2^53−1) throws on the way out.
    // 4611686018427387900n is ~2^62; doubled stays inside i64 but far outside
    // the safe-number range (the task's 4611686018427387910n * 2 overflows i64).
    let src = r#"export func big(x: int) -> int
    return x * 2
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./int_unsafe_out.mjs";

try {
    lib.big(4611686018427387900n);
    console.log("FAIL: did not throw");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof RangeError, String(e));
}

console.log("ALL OK");
"#;
    assert_ok(&run_driver("int_unsafe_out", src, driver));
}

// ---- 5–6: text round-trip and lone surrogate --------------------------------

#[test]
fn text_round_trip_and_lone_surrogate() {
    // Bullets 5–6: normal/astral strings round-trip; lone surrogate → SudoTrap.
    let src = r#"export func echo_text(s: text) -> text
    return s
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./text_rt.mjs";
import { SudoTrap } from "./_sudo_rt.mjs";

// ASCII
const a = lib.echo_text("hello");
assert.equal(typeof a, "string");
assert.equal(a, "hello");

// Non-ASCII BMP + astral (emoji, surrogate pair)
const b = lib.echo_text("café 🦀");
assert.equal(typeof b, "string");
assert.equal(b, "café 🦀");

// Lone high surrogate → InvalidConvert trap
try {
    lib.echo_text("\uD800");
    console.log("FAIL: did not throw");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof SudoTrap, String(e));
    assert.equal(e.kind, "InvalidConvert");
}

console.log("ALL OK");
"#;
    assert_ok(&run_driver("text_rt", src, driver));
}

// ---- 7: List defensive copies ----------------------------------------------

#[test]
fn list_round_trip_defensive_copies() {
    // Bullet 7: in-copy independent of host array; out-copy not shared/cached.
    let src = r#"export func list_id(items: List<int>) -> List<int>
    return items
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./list_copy.mjs";

const host = [1, 2, 3];
const first = lib.list_id(host);
assert.deepEqual(first, [1, 2, 3]);

// Mutate host after call — returned array must be unaffected.
host.push(99);
assert.deepEqual(first, [1, 2, 3]);
assert.deepEqual(host, [1, 2, 3, 99]);

// Mutate the returned array, then call again with a fresh input.
first.push(77);
const second = lib.list_id([10, 20]);
assert.deepEqual(second, [10, 20]);
assert.deepEqual(first, [1, 2, 3, 77]);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("list_copy", src, driver));
}

// ---- 8: Map defensive copies -----------------------------------------------

#[test]
fn map_round_trip_defensive_copies() {
    // Bullet 8: Map in/out copies; plain object accepted for text keys.
    let src = r#"export func map_text_id(m: Map<text, int>) -> Map<text, int>
    return m

export func map_int_id(m: Map<int, int>) -> Map<int, int>
    return m
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./map_copy.mjs";

// Native Map input (text keys)
const hostMap = new Map([["a", 1], ["b", 2]]);
const out1 = lib.map_text_id(hostMap);
assert.ok(out1 instanceof Map);
assert.equal(out1.get("a"), 1);
assert.equal(out1.get("b"), 2);
assert.equal(out1.size, 2);

hostMap.set("c", 3);
assert.equal(out1.has("c"), false);
assert.equal(out1.size, 2);

out1.set("z", 99);
const out2 = lib.map_text_id(new Map([["x", 7]]));
assert.equal(out2.size, 1);
assert.equal(out2.get("x"), 7);
assert.equal(out2.has("z"), false);

// Plain object input for text-keyed maps
const hostObj = { p: 10, q: 20 };
const out3 = lib.map_text_id(hostObj);
assert.ok(out3 instanceof Map);
assert.equal(out3.get("p"), 10);
assert.equal(out3.get("q"), 20);
hostObj.r = 30;
assert.equal(out3.has("r"), false);

// int-keyed Map
const intHost = new Map([[1, 100], [2, 200]]);
const out4 = lib.map_int_id(intHost);
assert.equal(out4.get(1), 100);
assert.equal(out4.get(2), 200);
intHost.set(3, 300);
assert.equal(out4.has(3), false);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("map_copy", src, driver));
}

// ---- 9: Set defensive copies -----------------------------------------------

#[test]
fn set_round_trip_defensive_copies() {
    // Bullet 9: Set in/out copies are independent of host and of prior returns.
    let src = r#"export func set_id(s: Set<int>) -> Set<int>
    return s
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./set_copy.mjs";

const host = new Set([1, 2, 3]);
const first = lib.set_id(host);
assert.ok(first instanceof Set);
assert.equal(first.size, 3);
assert.ok(first.has(1) && first.has(2) && first.has(3));

host.add(99);
assert.equal(first.has(99), false);
assert.equal(first.size, 3);

first.add(77);
const second = lib.set_id(new Set([10, 20]));
assert.equal(second.size, 2);
assert.ok(second.has(10) && second.has(20));
assert.equal(second.has(77), false);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("set_copy", src, driver));
}

// ---- 10: Tuple round-trip --------------------------------------------------

#[test]
fn tuple_round_trip() {
    // Bullet 10: fixed tuples surface as JS arrays with converted slots.
    let src = r#"export func make_pair(a: int, b: bool) -> (int, bool)
    return (a, b)

export func two_sum(nums: List<int>, target: int) -> Option<(int, int)>
    seen = Map()
    for i = 0 to nums.length - 1
        complement = target - nums[i]
        if seen.has(complement)
            return Some((seen[complement], i))
        seen[nums[i]] = i
    return None
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./tuple_rt.mjs";

const p = lib.make_pair(7, true);
assert.ok(Array.isArray(p));
assert.equal(p.length, 2);
assert.equal(typeof p[0], "number");
assert.equal(p[0], 7);
assert.equal(typeof p[1], "boolean");
assert.equal(p[1], true);

const hit = lib.two_sum([2, 7, 11, 15], 9);
assert.ok(Array.isArray(hit));
assert.equal(hit.length, 2);
assert.equal(hit[0], 0);
assert.equal(hit[1], 1);

const miss = lib.two_sum([1, 2, 3], 100);
assert.equal(miss, null);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("tuple_rt", src, driver));
}

// ---- 11: Record plain-object defensive copies ------------------------------

#[test]
fn record_round_trip_defensive_copies() {
    // Bullet 11: records cross as plain objects, not generated class instances.
    let src = r#"record Point
    x: int
    y: int

export func point_id(p: Point) -> Point
    return p
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./record_copy.mjs";

const host = { x: 3, y: 4 };
const first = lib.point_id(host);
assert.equal(typeof first, "object");
assert.ok(first !== null);
assert.equal(first.x, 3);
assert.equal(first.y, 4);
// Plain object — not an instance of the internal Point class.
assert.equal(Object.getPrototypeOf(first), Object.prototype);

host.x = 99;
assert.equal(first.x, 3);

first.y = 77;
const second = lib.point_id({ x: 1, y: 2 });
assert.equal(second.x, 1);
assert.equal(second.y, 2);
assert.equal(first.y, 77);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("record_copy", src, driver));
}

// ---- 12: Enum tagged-object round-trip -------------------------------------

#[test]
fn enum_tree_round_trip() {
    // Bullet 12: recursive enum uses tagged {"$": "Variant", ...fields} objects.
    let src = r#"enum Tree
    Leaf
    Node(value: int, left: Tree, right: Tree)

export func insert(t: Tree, v: int) -> Tree
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

export func contains(t: Tree, v: int) -> bool
    match t
        case Leaf
            return false
        case Node(value, left, right)
            if v < value
                return contains(left, v)
            else if v > value
                return contains(right, v)
            else
                return true

export func to_sorted_list(t: Tree) -> List<int>
    match t
        case Leaf
            return []
        case Node(value, left, right)
            return to_sorted_list(left) + [value] + to_sorted_list(right)
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./enum_tree.mjs";

const leaf = { "$": "Leaf" };
const root = { "$": "Node", value: 5, left: leaf, right: leaf };

const t1 = lib.insert(root, 3);
assert.equal(t1.$, "Node");
assert.equal(t1.value, 5);
assert.equal(t1.left.$, "Node");
assert.equal(t1.left.value, 3);
assert.equal(t1.left.left.$, "Leaf");
assert.equal(t1.left.right.$, "Leaf");
assert.equal(t1.right.$, "Leaf");

const t2 = lib.insert(t1, 8);
assert.equal(t2.right.$, "Node");
assert.equal(t2.right.value, 8);

assert.equal(lib.contains(t2, 3), true);
assert.equal(lib.contains(t2, 8), true);
assert.equal(lib.contains(t2, 4), false);
assert.deepEqual(lib.to_sorted_list(t2), [3, 5, 8]);

// Host-built tagged object round-trips through contains without mutation.
const hostTree = {
    "$": "Node",
    value: 1,
    left: { "$": "Leaf" },
    right: { "$": "Leaf" },
};
assert.equal(lib.contains(hostTree, 1), true);
assert.equal(hostTree.left.$, "Leaf");

console.log("ALL OK");
"#;
    assert_ok(&run_driver("enum_tree", src, driver));
}

// ---- 13: Option collapse ---------------------------------------------------

#[test]
fn option_collapses_null_and_accepts_nullish_in() {
    // Bullet 13: Some → bare value, None → null; null/undefined in → None.
    let src = r#"export func find(items: List<int>, t: int) -> Option<int>
    for i = 0 to items.length - 1
        if items[i] == t
            return Some(i)
    return None

export func opt_value_or_zero(x: Option<int>) -> int
    match x
        case Some(v)
            return v
        case None
            return 0
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./option_rt.mjs";

// Out: Some collapses to bare value; None → null
assert.equal(lib.find([5, 6, 7], 6), 1);
assert.equal(typeof lib.find([5, 6, 7], 6), "number");
assert.equal(lib.find([5, 6, 7], 9), null);

// In: null and undefined both map to None
assert.equal(lib.opt_value_or_zero(null), 0);
assert.equal(lib.opt_value_or_zero(undefined), 0);
// Bare value maps to Some
assert.equal(lib.opt_value_or_zero(42), 42);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("option_rt", src, driver));
}

// ---- 14: Result Err throws SudoError ---------------------------------------

#[test]
fn result_err_throws_sudo_error_with_payload() {
    // Bullet 14: Ok → bare value; Err → SudoError with converted payload.
    let src = r#"export func safe_div(a: int, b: int) -> Result<int, text>
    if b == 0
        return Err("division by zero")
    return Ok(a / b)
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./result_rt.mjs";
import { SudoError } from "./_sudo_rt.mjs";

assert.equal(lib.safe_div(10, 2), 5);
assert.equal(typeof lib.safe_div(10, 2), "number");

try {
    lib.safe_div(1, 0);
    console.log("FAIL: did not throw");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof SudoError, String(e));
    assert.equal(e.payload, "division by zero");
}

console.log("ALL OK");
"#;
    assert_ok(&run_driver("result_rt", src, driver));
}

// ---- 15: inout writeback ---------------------------------------------------

#[test]
fn inout_list_writeback_mutates_in_place() {
    // Bullet 15: inout List writeback mutates the caller's own array object.
    let src = r#"export func push_twice(items: inout List<int>, v: int)
    items.append(v)
    items.append(v)
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./inout_wb.mjs";

const items = [1];
lib.push_twice(items, 9);
// Same array object, mutated in place.
assert.deepEqual(items, [1, 9, 9]);

console.log("ALL OK");
"#;
    assert_ok(&run_driver("inout_wb", src, driver));
}

// ---- 16: Trap surfaces as SudoTrap with .kind ------------------------------

#[test]
fn trap_throws_sudo_trap_with_kind() {
    // Bullet 16: runtime traps reach the host as SudoTrap with a kind string.
    // MIN / -1 overflows through the adapted safe_div export (valid args).
    let src = r#"export func safe_div(a: int, b: int) -> Result<int, text>
    if b == 0
        return Err("division by zero")
    return Ok(a / b)

export func first(items: List<int>) -> int
    return items[0]
"#;
    let driver = r#"
import assert from "node:assert/strict";
import * as lib from "./trap_kind.mjs";
import { SudoTrap } from "./_sudo_rt.mjs";

// Overflow: i64 MIN / -1
try {
    lib.safe_div(-9223372036854775807n - 1n, -1n);
    console.log("FAIL: did not throw Overflow");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof SudoTrap, String(e));
    assert.equal(e.kind, "Overflow");
}

// OutOfBounds via empty-list index
try {
    lib.first([]);
    console.log("FAIL: did not throw OutOfBounds");
    process.exit(1);
} catch (e) {
    assert.ok(e instanceof SudoTrap, String(e));
    assert.equal(e.kind, "OutOfBounds");
}

console.log("ALL OK");
"#;
    assert_ok(&run_driver("trap_kind", src, driver));
}
