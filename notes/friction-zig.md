# Friction log: `backend_zig`

Notes from finishing the Zig backend against `spec/backend-guide.md`, the Rust
reference (`backend_rs`, closest match — same error-return trap model), the C
reference (per-monomorph copy/eq/canon helpers, structural map keys), IR, SDK,
and harness. Toolchain: **Zig 0.16.0**. Every open question from the task brief
is answered here.

Definition of done reached: `sudoc conformance --target zig` is 9/9; the full
6-target conformance is 9/9; `sudoc test examples/*.sudo examples/pitfalls/*.sudo
stdlib/*.sudo` is green except `order_dependent.sudo`'s
`test_diverges_first_key_by_iteration_order`, which diverges by design (Zig's
Map iteration order differs from the other targets — exactly what the harness
exists to surface).

---

## Zig 0.16 stdlib API reference (version-pinning notes)

**This section is the load-bearing part of the log.** Zig's stdlib churns hard
between releases; everything below was verified by direct `zig run` probes on
0.16.0 before use (see the task's "compile early and often" mandate). If you
bump the toolchain, re-probe these first — a compile error in the generated
runtime almost always traces back here.

| Concern | 0.16.0 API (verified) | Old / wrong-from-memory |
|---|---|---|
| General allocator | `std.heap.DebugAllocator` | `GeneralPurposeAllocator` (gone) |
| Arena | `std.heap.ArenaAllocator.init(std.heap.page_allocator)`; `arena.allocator()`; reset with `_ = arena.reset(.retain_capacity)` | `.deinit()`-per-use patterns |
| Growable list | `std.ArrayListUnmanaged(T)`, init `= .empty`, **allocator per call**: `list.append(alloc, v)`, `list.insert(alloc, i, v)`, `list.orderedRemove(i)`, `list.appendSlice(alloc, s)`, `list.ensureTotalCapacity(alloc, n)`; elements at `list.items` | `ArrayList(T).init(alloc)` (managed variant not used) |
| String-keyed map | `std.StringHashMapUnmanaged(V)`, init `= .empty`; `map.getOrPut(alloc, key)` → `.found_existing`, `.key_ptr`, `.value_ptr`; `map.getPtr(key)` → `?*V`; `map.contains(key)`; `map.remove(key)` → bool; `map.count()`; `map.valueIterator()` → `.next()` yields `*V` | managed `StringHashMap` |
| Checked int arith | `std.math.add/sub/mul(i64, a, b)` and `std.math.negate(a)` return catchable `error.Overflow` | — |
| Floor div/mod | `@divFloor`/`@mod` **panic uncatchably on divisor 0**, and a **comptime-known** `0/0` is a *compile error*. Guard `if (b == 0) return error.DivByZero;` first; guard `minInt/-1` for div | assuming they error |
| Float div | `a / b` with **comptime-known** `0.0 / 0.0` is a *compile error* ("division by zero here causes illegal behavior"). Route through a runtime `fn fdiv(a: f64, b: f64) f64 { return a / b; }` so operands are runtime values → IEEE NaN/Inf | inlining `0.0/0.0` in generated code |
| Round | `std.math.round` is **already ties-away-from-zero** (matches spec §7) | needing a `floor(x+0.5)` shim |
| min/max on floats | hand-rolled `fmin`/`fmax` (NaN-propagating, `min(-0.0,0.0) == -0.0`) — **not** `@min`/`@max` (which don't propagate NaN the spec way) | `@min`/`@max` |
| float↔int | `@intFromFloat` / `@floatFromInt` (guard NaN/range before `@intFromFloat` → InvalidConvert) | `@floatToInt`/`@intToFloat` (renamed) |
| stdout | **no clean `std.io.getStdOut` path** without new writer machinery — use `std.c.write(1, ptr, len)` and link libc (`-lc`) | `std.io.getStdOut().writer()` |
| exit | `std.process.exit(code)` | — |
| error name | `@errorName(err)` yields the bare variant name — and our trap kinds are the error-set names, so it *is* the reported kind, no mapping table | — |
| addr-of-fn | `&funcname` is `*const fn (...) E!R`; call a fn pointer directly `ptr(args)` | — |

### Hard Zig gotchas that bit the prior attempt / this one

1. **An unmutated `var` local is a hard compile error** (not a warning):
   `error: local variable is never mutated`. Emit `const` for never-reassigned
   locals, or append `_ = &name;` after each `var` (taking the address counts
   as a mutation-capable use and also suppresses *unused-variable*). The
   backend uses the `var … ; _ = &name;` form for every emitted mutable local,
   loop binding, and param shadow.

2. **`break`/`continue` inside a `switch` inside a loop targets the LOOP, not
   the switch** (the opposite of C). So `match`-lowered-to-`switch` needs *no*
   labeled-loop workaround (backend-guide §4.5's C hazard does not apply to
   Zig). Verified with a 5-line probe (`hits == 2` for `break` at `i == 2`).

3. **Struct-literal construction uses `.field = value`, never `field: value`.**
   `field:` is *declaration* syntax; using it in a literal gives
   `error: expected '}', found ':'`. Record/enum/tuple *type decls* keep the
   colon; every *construction* (copy helpers, `NewRecord`, `NewVariant`,
   tuple exprs) uses `.f0 = …` / `.name = …`. This was the single most common
   bug while salvaging the stage-2 code.

4. **Zig forbids shadowing a function parameter** — you cannot write
   `var a = a;`. Params are also `const`, so a body that reassigns a param, or
   mutates its field/element, or passes it as an `inout` arg, needs a rename +
   shadow: emit the param as `a_arg` and open the body with
   `var a = a_arg; _ = &a;`. Only params the body actually *writes* get this
   treatment (see `written_params`) — read-only params stay clean `a: T`.

5. **Zig `switch` must be exhaustive, and a redundant `else` is an error**
   (`error: unreachable else prong; all cases already handled`). Enum/Result
   matches are checker-exhaustive, so emit `else` *only* for a sudo wildcard
   arm. `i64` switches always need `else`; `bool` needs one unless both `true`
   and `false` are covered explicitly.

6. **Nested functions do NOT close over outer locals** (there is no
   free-variable capture in Zig, ever — this is not a version quirk). A
   `struct { fn f() {...} }` defined in a function body cannot see that
   function's runtime locals; referencing one is `error: 'x' not accessible
   from here ... crosses namespace boundary`, and reusing a local's name as a
   param is `error: parameter 'x' shadows local variable from outer scope`. Any
   code-lowering that needs to run a *block* with access to enclosing locals
   must stay in-scope — use an **inline labeled block**, not a nested fn (see
   `expect_trap` below). This one silently passes `conformance` (whose
   `expect_trap` blocks are all self-contained) but is caught by the harness
   lockstep fixtures — always run the full `cargo test --workspace`, not just
   `sudoc conformance`.

---

## Task-brief design decisions (as implemented)

### `name() = "zig"`, multi-module, one `.zig` per sudo module

`emit_program` writes deps first (`emit(m, with_tests=false, is_entry=false)`)
then the entry (`is_entry=true`). Cross-module IR names `dep.fn` stay `dep.fn`
in Zig (an `@import("dep.zig")` const named `dep` + a `pub fn fn`). Every
generated decl is `pub` so cross-module use just works. The runtime is
`sudo_rt.zig`, imported as `const rt` everywhere. The corpus has no `import`
today, so the multi-module path is lightly exercised but symmetric with the
Rust backend's layout.

### `test_recipe`

Verified invocation names the output `sudo_tests`:
`zig build-exe {entry}.zig -femit-bin=sudo_tests -lc -O ReleaseSafe`, run
`./sudo_tests`. `-lc` is required because stdout goes through `std.c.write`.

### Traps: closed error set, `!T`, `try`, local observation

`error{OutOfBounds, KeyMissing, DivByZero, Overflow, UnwrapFailed,
InvalidConvert, InvalidArg, AssertFailed}`. Functions return `rt.SudoError!T`;
calls are `(try callee(...))`; the assert-operand diagnostic goes in a global
fixed buffer (`sudo_trap_detail`) exactly like C's. `@errorName` is the
reported kind.

### `expect_trap` — the interesting one (no closures in Zig)

C uses inline `setjmp`; Rust uses `catch_unwind` over a closure. Zig has
**neither** — and `try` inside an inline block returns from the *enclosing*
function, not a local scope, so you cannot just wrap the body.

**Zig land mine (cost a full rewrite of this feature): nested functions do NOT
close over outer locals.** Free-variable capture is not a concept that exists
in Zig — a `struct { fn run() {...} }` defined inside a function body cannot see
the enclosing function's runtime locals at all. The first attempt lowered the
`expect_trap` body into such a nested `fn` and tried to thread referenced outer
locals in as pointer params. Two ways that fails, both caught by the harness
lockstep tests (`conformance/semantics/traps.sudo` hid the bug because every
one of its blocks declares its own locals — but two pre-existing lockstep
fixtures reference an enclosing local):

1. `error: function parameter 'a' shadows local variable from outer scope` —
   the threaded param reused the sudo local's name and collided.
2. `error: mutable 'a' not accessible from here ... crosses namespace boundary`
   — any outer local the free-var analysis *missed* (e.g. a mutating method on
   a bare-var receiver, `a.pop()`) was referenced directly across the fn
   boundary. There is no `a` to reference; nested fns are a hard namespace wall.

**Resolution — inline labeled block, zero capture.** A Zig labeled block is an
*ordinary scope*, not a namespace boundary, so the body sees enclosing locals
exactly as normal code does. Lower the body inline into a block that yields
`?rt.SudoError`; every fallible op breaks to the label carrying the error
instead of `try`/`return`-ing out of the test:

```zig
const _sudo_trap: ?rt.SudoError = _sudo_et: {
    var x: i64 = (a.pop() catch |_sudo_e| break :_sudo_et _sudo_e);   // outer `a`, no capture
    (rt.assertEqI64(x, 0, 5) catch |_sudo_e| break :_sudo_et _sudo_e);
    break :_sudo_et null;                                             // fell through
};
if (_sudo_trap) |e| {
    if (e != rt.SudoError.OutOfBounds) return rt.expectTrapWrong(3, "OutOfBounds", @errorName(e));
} else return rt.expectTrapNone(3, "OutOfBounds");
```

Mechanically: a `trap_label: Option<String>` on the emitter switches every
`try X` → `(X catch |e| break :label e)` (helper `tryx`/`try_line`) and every
trap-`return ERR` → `break :label ERR` (helper `raise`) while emitting the
body. `SudoError` coerces to the block's `?SudoError` result; unlabeled `break`
inside a `while true` still targets the loop, so the two don't collide (probed).
Mutations to outer locals up to the trap point persist (matches C/Rust). No
free-variable analysis, no capture, no rename — the body is emitted with locals
referenced verbatim.

### Memory: ONE global arena, reset between tests, no per-value frees (v1)

`sudo_arena = std.heap.ArenaAllocator.init(std.heap.page_allocator)` at global
scope. Every sudo heap value — list buffers, boxed enum/Option/Result payloads,
map keys and entries — allocates from `rt.allocator()`. The TAP runner does
`_ = sudo_arena.reset(.retain_capacity)` after each test. A trap abandons
in-flight values mid-mutation; the next reset reclaims them, so there are **no
per-value frees** and no leaks across tests. This is the deliberate v1 strategy
(cf. C's intrusive-list free-on-trap, which is more work than v1 needs).

**Read-only composite params are borrowed, not copied.** A non-`inout` param
whose type is managed (`needs_dup`), that the body never writes, on a function
that is never taken as a value (`FuncRef`), is emitted as `*const T` and the
call site passes `&arg` instead of a deep copy. Escaping uses still go through
`store()` and deep-copy on read. Values that *do* get copied (returns, list/
record stores, by-value params of non-eligible or address-taken functions)
still live until the next arena reset — only the hot "read-only composite in a
loop" path no longer allocates per call. *v2 upgrade path:* thread allocations
for scoped freeing, or switch to a tracking/debug allocator in a leak-check
build — not required by the corpus.

### Numbers

`int` = `i64`, checked through `rt.add/sub/mul/neg` (→ `Overflow`), with
`divFloor`/`modFloor` guarding zero divisor (`DivByZero`) and `minInt/-1`
(`Overflow`) *before* `@divFloor`/`@mod` (which would otherwise panic
uncatchably). `float` = `f64`; `+ - *` inline, `/` through `rt.fdiv` (see API
table — comptime `0.0/0.0` is a compile error). `bool` = `bool`.

`filled(n, v)` traps `InvalidArg` when `n < 0` (an explicit guard — the naive
`while (i < n)` loop would silently return `[]`; `conformance/semantics/
traps.sudo`'s `test_negative_filled` requires the trap).

### `for i = a to b` at `i64::MAX`

Flag-shape with wrapping increment (from the salvage, verified against
`loops.sudo`'s "single iteration at int max"):

```zig
const lo: i64 = …; const hi: i64 = …;
var i: i64 = lo; _ = &i;
var done = !(lo <= hi); _ = &done;
while (!done) : (i +%= 1) {
    done = (i == hi);
    // body
}
```

`+%=` wraps at `INT64_MAX` instead of panicking; the `done` flag is set at the
top of the body so the loop terminates after the single iteration at the bound.
`continue` cannot skip the flag update (it's in the header slot, per §4.5).

### Floats

Runtime `fmin`/`fmax` (NaN-propagating; `min(-0.0, 0.0) == -0.0` via signbit),
`round` = `std.math.round` (already ties-away), and a custom stable insertion
sort with `f64SortLt` (NaN last, `-0.0` before `+0.0`). All verified against
`conformance/semantics/floats.sudo`. `List<int>` uses a plain ascending
insertion sort.

### Map/Set: structural keys via canonical byte encoding

Like the JS `key_form` / C structural keys. Each Map/Set is
`rt.SudoMap(K, V, appendKey)` / `rt.SudoSet(E, appendKey)` over a
`StringHashMapUnmanaged`. `appendKey` is a per-key-type function that writes an
injective encoding into a shared scratch buffer: scalars route to
`rt.key_i64`/`rt.key_bool`; composites get generated `keyapp_<mangle>` encoders
(`L[…]`, `R{name:…}`, `E<variant>(…)`, `(…)`, `S(…)`/`N`, `K(…)`/`X(…)` — tags
chosen for injectivity). Stored keys are arena-`dupe`d so they outlive the
scratch buffer; lookups reuse the transient slice (StringHashMap compares by
bytes). The map stores `(original key, value)` so iteration yields original
keys. Map/Set **equality is order-insensitive** (count + membership walk).
`copy_*` deep-copies entries; `canon_*` iterates in native order
(diagnostic-only per lockstep v1 — canon detail is not part of the harness
verdict, only the trap kind is).

### Records → structs, enums → `union(enum)`, match → `switch`

Enum payloads box `Record/Enum/Option/Result/Tuple` fields as `*const T`
(same rule as `backend_c::boxed_in_payload`); construction wraps `rt.box`,
match unboxes with `p.*`. `Option<T>` maps to Zig's native `?T` (payload boxed
when composite); `Result` to a `union(enum){ Ok, Err }`. Option match lowers to
`if (x) |p| … else …`; enum/Result to `switch`.

---

## §4 land-mine catalog — where Zig landed

- **§4.1 evaluation order / aliasing.** Zig's method calls auto-ref the
  receiver, and the arg/receiver aliasing that forces C/Rust temporaries did
  **not** bite the corpus here: mutating list/map methods take the receiver as
  an lvalue (`b.append(v)`), and index-assign materializes key/value through
  `store()` before the `.put`. The one real trap was **receiver form**:
  `place_ptr` returns `&b`, and `try &b.append(3)` parses as `try &(b.append(3))`
  — method receivers must be the *lvalue* (`b`, Zig auto-refs), while only the
  free-fn `sortI64` gets a real `&b` pointer.
- **§4.2 short-circuit.** `and`/`or` are lazy in Zig; trapping RHSs are still
  statement-lowered into an `if` when `can_trap(rhs)` (`emit_lazy_bool`).
- **§4.3 inout.** Native pointers: inout params are `*T`, reads emit `n.*`,
  method receivers use `n` directly (already a pointer). No writeback tuples.
- **§4.5 loops.** Covered above (INT64_MAX flag; `break` targets the loop so no
  labeled jumps; for-in snapshots the collection and binds fresh owned copies).
- **§4.7 numbers / §4.8 hashing.** Covered above. Map iteration order is Zig's
  native `StringHashMap` order — deliberately different from other targets, so
  `order_dependent.sudo` diverges as intended.
- **§4.11 host-builtin shadowing.** The runtime is reached only through the
  unshadowable `rt.` import; user identifiers land in the module namespace but
  cannot touch `rt`'s. `stdlib/bigint.sudo`'s `record BigInt` is just a struct
  named `BigInt` — no Zig builtin clash.
- **§4.13 uncatchable traps.** `StackOverflow` is unobservable (a real Zig stack
  overflow aborts the process; there is no catch). No synthetic depth counter
  was added — it would diverge from "what the language does". **Corpus check:**
  no test exercises catchable stack overflow, so this is a known soft spot, not
  a present failure (same carve-out as C/Rust). All *other* trap kinds route
  through catchable error returns; no generated operation can fail via an
  uncatchable abort (int arithmetic, div/mod, float→int, list/map bounds are all
  guarded).

---

## Salvage: kept vs rewrote

**Kept largely intact** (verified correct): the scalar/arith/control-flow
emitter (stage 1), the `SudoList` wrapper and int/float insertion sorts, the
`copy_/eq_/canon_` scaffolding for List/Tuple/Record/Enum/Option/Result, the
assert-detail buffer + `det_*` helpers, the INT64_MAX for-range shape, enum
payload boxing, and the checked-arith/float runtime helpers.

**Fixed in the salvage** (was compiling-but-unverified / mid-write):
- struct-literal construction used `field:` (decl syntax) → invalid Zig;
  changed every construction to `.field =`.
- mutating method receivers used `&b` → `try &b.append(x)`; switched to lvalue
  receivers, keeping `&`/pointer only for `sortI64`/`sortF64`.
- enum/Result matches emitted a redundant `else` → `unreachable else prong`
  compile error; now emitted only for wildcard arms.
- `box` returned `*T`; payload slots are `*const T` — changed to `*const T`.

**Newly written** (was deferred / TODO in the salvage):
- the TAP runner `main` + `rt.run_tests`/`TestCase` (the salvage had print
  primitives but no runner and no `main`, so `build-exe` couldn't link).
- Map/Set end to end: `SudoMap`/`SudoSet`, key encoders (`keyapp_*`,
  `collect_key_types`), all Map/Set builtins, map index read/write, for-in over
  Map/Set, and Map/Set copy/eq/canon.
- `expect_trap` (inline labeled block; see the dedicated section — the
  nested-fn capture approach was a dead end because Zig nested fns don't
  close over locals).
- `CallValue` and `FuncRef` as `&fn` pointers (generics / higher-order).
- list/text concatenation (`+`), which the salvage hit as
  `unreachable!("handled above")` on `bst.sudo`.
- `written_params` analysis + mutable param shadowing (Zig const params).
- Read-only composite params → `*const T` borrow (no deep-copy at call site).
- `fdiv` + the `filled` negative-arg guard.

---

## Process / registration

Registered alongside the existing five backends (not replacing any): added
`crates/backend_zig` to `sudoc/Cargo.toml` members, `sudoc-backend-zig` to
`harness/Cargo.toml`, and `Box::new(sudoc_backend_zig::ZigBackend)` to
`all_backends()` (order: py, c, js, swift, rs, zig). Removed the leftover
`tests/_verify_stage1.rs` scratch file; execution tests live in
`tests/execute.rs` (mirrors `backend_rs`, drives `zig build-exe`, skips
gracefully if `zig` is not on PATH).
