# Friction log: `backend_js`

Notes from implementing the JavaScript backend against `spec/backend-guide.md`,
the Python reference (`backend_py`), IR, SDK, and harness. Every open
"verify/confirm/check" from the task brief is answered here.

---

## Environment / process friction

### Shell Execute tool is unusable for mutations in this sandbox

Any mutating shell (`mkdir`, redirects, `cargo`, `node`) was silently cancelled.
Only the Write/Edit tools could create files (they create parent directories).
Verification (`cargo test`, `sudoc conformance`, etc.) must be run outside the
agent session. **What would have helped:** the task brief or system prompt
stating upfront that Execute is read-only so time is not spent rediscovering
this.

### Permission / acceptEdits mode

Earlier turns stalled on cancelled tool calls before the "Write/Edit only"
instruction. The working mode is: write complete sources via Write/Edit; do not
attempt local build verification in-session.

### JS operator precedence ≠ Python (caught in conformance)

Initial port copied Python's precedence numbers for `not` (tier 3, looser than
comparisons). That is **wrong for JS**: `!` is unary-tier and binds tighter
than every binary operator. Emitted `_rt.sudo_assert(!nan === nan, 6)` parsed
as `(!nan) === nan`, and hoisted while-guards `if (!_sudo_h0 <= 30n)` parsed
as `(!_sudo_h0) <= 30n` (false ≤ 30n is true after coercion → loop exits early).

**Fix:** JS table (low→high): `||`=1, `&&`=2, `===`/`!==`=3, relational=4,
`+`/`-`=5, `*`/`/`/`%`=6, unary `!`/`-`=7, atom=9. Equality is a **separate
looser tier** than relational in JS (unlike Python, where they share a tier).

### User record `BigInt` shadows the JS global (stdlib/bigint.sudo)

`stdlib/bigint.sudo` defines `record BigInt`. The emitter correctly produces
`export class BigInt { ... }` and `new BigInt(...)` for construction. But list
`.length` was lowered as `BigInt(xs.length)` to coerce Number→BigInt — in the
same module that **shadows** the built-in, so JS tries to call the *class*
without `new` and throws
`Class constructor BigInt cannot be invoked without 'new'`.

**Fix:** always reach host coercions via `globalThis.BigInt` /
`globalThis.Number` / `globalThis.Math` so user type names cannot shadow them.
Literals (`5n`) and `_sudo_rt.mjs` (separate module) were already fine.

### Entry-module path guard vs macOS temp_dir symlinks

`std::env::temp_dir()` on macOS often lives under `/var/...` which realpaths to
`/private/var/...`. Spawning `node` with an absolute non-realpathed argv[1]
made `path.resolve(process.argv[1]) !== fileURLToPath(import.meta.url)`, so
`run_tests` never ran and execute tests spuriously passed. Fixed by (a) execute
tests using `.current_dir(dir)` + relative entry (harness shape), and (b)
`fs.realpathSync(path.resolve(process.argv[1]))` in the generated guard.

---

## Spec / guide open questions (resolved)

### 4.1 Evaluation order (JS vs C)

**Finding:** JS specifies left-to-right evaluation for function arguments,
binary operands, and array elements (ECMAScript). Unlike C, we do **not** need
the temporary-materialization trick used by `backend_c`. Emission follows the
same left-to-right walk as `backend_py` (`expr`/`store` of children in source
order). No reordering for "readability" was introduced.

**What would have helped:** a one-liner in backend-guide §4.1 naming languages
with defined LTR order (JS, Python, Java) vs unspecified (C, C++).

### 4.2 Short-circuit + effects

**Finding:** JS native `&&` / `||` short-circuit left-to-right like sudo
`and`/`or`. Frontend hoisting means inout-passing calls only appear as
statement roots / sole RHS, so they never sit nested inside `&&`/`||` in the
IR the emitter sees. The `hoisting.sudo` pattern / `hoisted_inout_calls_behave`
execute test exercises "short circuit skips mutation" via hoisted form; direct
`lhs && rhs` emission is correct.

### 4.5 `for i = a to b` and i64 MAX

**Finding:** The guide's C land-mine (wrap on `i++` past `INT64_MAX`) does
**not** apply. JS BigInt has no fixed width. Loop shape:

```js
const _sudo_from_i = from;
const _sudo_to_i = to;
for (let i = _sudo_from_i; i <= _sudo_to_i; i += 1n) { ... }
```

After the last body with `to == 2n**63n-1n`, `i += 1n` yields `2n**63n`, the
condition fails, and the loop ends. No checked arithmetic on the increment
(using `_rt.chk` there would falsely Overflow). Covered by
`for_range_to_i64_max_terminates` in `tests/execute.rs`.

### 4.7 BigInt as `sudo int` (mandatory)

Plain `number` loses precision above 2^53 (`9223372036854775807` is the classic
trap). Strategy mirrors Python's arbitrary-precision + `chk` range gate:

- Literals: `5n`, `-1n`, `9223372036854775807n`
- Arithmetic: native BigInt ops then `_rt.chk`
- Floor div/mod: JS BigInt `/` and `%` truncate toward zero / sign-of-dividend;
  runtime converts to floor semantics (same as Python `//` and `%`)
- List indices: bounds-check in BigInt space against `BigInt(a.length)`, then
  `Number(i)` for the actual index (never convert first — huge BigInts can
  become surprising finite Numbers)

### Float helpers vs host builtins

- `Math.min`/`Math.max`: NaN propagation is engine-dependent enough that we
  always use explicit `_rt.fmin`/`fmax` (NaN propagates; `min(-0,0)==-0`).
- `Math.round`: half-toward-+∞, not ties-away-from-zero; use
  `_rt.round_half_away` (`floor(x+0.5)` / `ceil(x-0.5)`).
- `int_of`: `Math.trunc` → `BigInt` → range check against i64 bounds; NaN/Inf
  → `InvalidConvert`.

### Deep equality / bool-vs-int (Python `eq` quirk)

Python's `eq` treats bool specially because `bool` subclasses `int` and
`True == 1`. In JS, bool is `boolean` and int is `BigInt`; they never conflate.
That branch is unnecessary. Scalar floats use `===` (already `NaN !== NaN`).
Composites walk via `_rt.eq`.

### Float keys on Map/Set

backend-guide §4.8: floats are never keys. Type checker enforces this; runtime
`key_form` still encodes numbers stably if seen, but no defensive trap.

### `canon` float formatting

Python `repr(1.0) == "1.0"`; JS `String(1.0) === "1"`. **Choice:** force
trailing `.0` for integral finite floats so assert diagnostics stay closer
across targets. `-0` via `Object.is(x, -0)` → `"-0.0"`. NaN/Inf match Python
(`"NaN"`, `"Inf"`, `"-Inf"`). Canon is diagnostic-only for lockstep (kinds
compare, not detail strings).

### Text literals

IR has `IrExprKind::Text(Vec<i64>)` already as Unicode scalars. No host
string codec needed (adapters out of scope). Emit `[97n, 98n, 99n]` directly
instead of Python's `_rt.text("abc")`.

### Host adapters

Out of scope per task. `emit_program` only emits `_{module}_impl.mjs` +
runtime. No `api_file` / `emit_api`.

### Cross-module names

IR uses `"module.func"` in `CallFunc`/`Const`/`FuncRef`. Emit:

```js
import * as dep from "./_dep_impl.mjs";
// ...
dep.fn(...)
```

Namespace import property access matches the dotted IR name. All top-level
funcs/records/enums/consts are `export`ed so dependencies can see them.
Local `m.func(name)` still only resolves same-module names (same as Python);
inout writeback only applies to local callees — frontend guarantees that for
inout-passing calls.

### Test collection (no `globals()`)

JS modules do not expose a globals dict. **Choice:** emit an explicit array of
`[name, fn]` pairs in declaration order and pass it to `_rt.run_tests`. Main
gate:

```js
import { fileURLToPath } from "node:url";
import path from "node:path";
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    process.exit(_rt.run_tests([["test_foo", test_foo], ...]));
}
```

`path.resolve` is required because `process.argv[1]` may be relative while
`fileURLToPath(import.meta.url)` is absolute.

### Match lowering

**Choice:** `if/else if` + `instanceof` (not `switch`) so `break`/`continue`
inside a match arm inside a loop target the loop. Records/enums are ES classes
with `static _sudoKind` / `static _sudoFields` for `canon`/`dup`/`eq`/`key_form`.

### inout multi-return

Single component: bare return (`return x`, `n = bump(n)`). Multiple:
array + destructure (`return [ret, x]`, `[y, n] = take(n)`). Mirrors Python
tuple unpacking.

### Tuples

Represented as JS arrays (same as lists). Destructuring uses `[a, b] = ...`.
`needs_dup` for tuples if any element needs dup (same as Python).

### List length / map size as BigInt

`items.length` is a Number; sudo type is `int` → emit `BigInt(xs.length)` and
`BigInt(m.size)`.

### Reserved identifiers / `_sudo` prefix

Sudo identifiers are lexer idents; reserved words are separate token kinds
(syntax lexer comment). No corpus use of JS reserved words as idents observed.
Temps use `_sudo_` prefix. Did not find an upstream hard ban on user names
starting with `_sudo`, but the reserved-prefix convention is documented in the
guide; we rely on it for temps (`_sudo_sc`, `_sudo_from_*`, `_sudo_r`).

### Stack overflow mapping

Python maps `RecursionError` → `StackOverflow` in TAP. JS: `RangeError` whose
message includes `"call stack"` is mapped the same way in `run_tests`.

### key_form encoding

JSON.stringify of a tagged recursive structure (`["i","5"]`, `["a",[...]]`,
class name + fields, Option/Result tags). Native `Map`/`Set` cannot key by
structural list equality; string keys + retained original key values match
Python's `(dup(k), v)` under `key_form(k)`.

### Sorting

`Array.prototype.sort` is stable since ES2019. Default comparator is wrong for
numbers/BigInt — always pass an explicit comparator (NaN last, -0 before +0).

---

## IR / SDK notes

- `Backend::emit_program`: deps first, entry last; `with_tests` only on entry
  (same as Python).
- `test_recipe`: no build steps; `["node", "_{entry}_impl.mjs"]`.
- Test names **must** come from `sudoc_ir::names::test_fn_names` — harness
  aligns outcomes by exact name.
- TAP lines: `ok N - name` / `not ok N - name [Kind]` /
  `not ok N - name [Kind: detail]`; exit nonzero on any failure; summary
  `# passed/total passed` for humans.

---

## Intentional divergences from Python backend

| Area | Python | JS |
|------|--------|-----|
| int | arbitrary int + chk | BigInt + chk |
| floor div | `//` | custom on BigInt |
| bool/int equality | special-case | unnecessary |
| match | `match`/`case` | `if`/`instanceof` |
| test discovery | `globals()` | explicit `[name,fn]` list |
| text lit | `_rt.text("...")` | `[codepoint]n` array |
| list append | `.append` | `.push` |
| map index | `m[k]` | `m.get`/`m.set` |
| host API | `emit_api` | omitted (OOS) |

---

## Files touched

**Created**

- `sudoc/crates/backend_js/Cargo.toml`
- `sudoc/crates/backend_js/src/lib.rs`
- `sudoc/crates/backend_js/src/runtime/_sudo_rt.mjs`
- `sudoc/crates/backend_js/tests/emit.rs`
- `sudoc/crates/backend_js/tests/execute.rs`
- `FRICTION.md` (this file)

**Modified (only these)**

- `sudoc/Cargo.toml` — workspace member `crates/backend_js`
- `sudoc/crates/harness/Cargo.toml` — dep `sudoc-backend-js`
- `sudoc/crates/harness/src/lib.rs` — `JsBackend` in `all_backends()`
