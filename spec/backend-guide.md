# Writing a sudo Backend

The handbook for adding a target language. Everything here was learned the
hard way while building the Python and C reference backends; follow the
porting order and the land-mine catalog and you will spend your time on your
language's idioms instead of rediscovering semantics bugs.

**Definition of done**: `sudoc conformance --target <yours>` is green — every
module in `conformance/semantics/` agrees with the reference backends under
the lockstep harness. That corpus *is* the semantics of sudo, executable.

---

## 0. Two front doors, one gate

There are two equal ways to implement a backend, and everything in this
guide except the Rust specifics applies to both:

- **In-tree**: implement the `sudoc_sdk::Backend` trait in a Rust crate,
  register it in the harness registry. Ships inside the `sudoc` binary.
- **External**: implement the [wire protocol](protocol.md) — a manifest plus
  an executable in *any* language that reads typed IR as JSON and returns
  generated files. Auto-discovered from `backends/*/*.sudoc-backend.json`
  under the working directory (or named explicitly with `--external`), and
  addressable with `--target <name>` like any built-in.

Neither door is privileged: the wire format is the same data contract the
in-tree backends are CI-verified against (byte-identical output through a
serialize→deserialize round trip), and every backend in this repo — hosted
either way — must be conformance-green on every push. The in-tree backends
are **reference implementations**, not incumbents: when a new backend runs
conformance, they are the oracles it is diffed against, and nothing stops an
independent external implementation of an *already-covered* language — give
it a distinct name (`myzig` next to `zig`) and lockstep will diff the two
implementations against each other, test by test.

Choosing a door for a new language comes down to four questions:

1. **Is the target language a good language for writing a compiler
   backend?** (ADTs, pattern matching, string assembly.) Haskell yes;
   C emphatically no — the C emitter generates per-instantiation
   `_copy`/`_free`/`_eq` machinery and wants a compiler language behind it.
2. **Does native hosting shrink where emission can run, or chain the
   emitter to an unstable toolchain?** An emitter written in Swift cannot
   run where swiftc doesn't exist (today Linux can still *emit* Swift); an
   emitter written in pre-1.0 Zig breaks on toolchain churn even when sudo
   didn't change. Both argue in-tree — and both verdicts are time-indexed,
   not eternal.
3. **Does emitting idiomatic output require native-speaker judgment?** The
   Haskell backend's hardest bugs (recursive-`let` black holes, laziness
   deferring traps across test boundaries) were found *by writing Haskell,
   in Haskell*, with GHC participating. That asymmetry favored external.
4. **Who maintains it, and in what language are they fluent?** Core-
   maintained backends benefit from rustc pointing at every match arm when
   the IR changes; a community maintainer's fluency beats that coupling.
   Hosting follows the maintainer — and a backend may migrate between doors
   without its target's standing changing in any way.

---

## 1. What a backend is

Four things (lockstep.md §5.3), packaged behind the `sudoc_sdk::Backend`
trait:

1. **A type mapping.** Every sudo type → a concrete host representation.
   You never see generics (monomorphization happened), inference variables,
   or the `text` alias (erased to `List<int>`; the surface type survives on
   export signatures for your host-boundary adapter).
2. **Value-semantics copy points.** Assignment, argument passing, returning,
   and container insertion behave as deep copies; mutation through one
   variable is never observable through another. You choose the mechanism
   (defensive copies, copy-on-write, persistent structures) — the corpus
   checks the observable behavior.
3. **A trap surface.** The closed trap-kind set (spec §8) must abort the
   current test / boundary call and be reported by kind.
4. **A test runner.** Generated test artifacts print the outcome protocol
   (`ok N - name` / `not ok N - name [Kind: detail]`, names from
   `sudoc_ir::names::test_fn_names`) and exit nonzero on any failure.

The trait itself (see `crates/sdk`):

```rust
impl Backend for JsBackend {
    fn name(&self) -> &str { "js" }
    fn emit_program(&self, modules: &[IrModule], with_tests: bool)
        -> Result<Vec<GeneratedFile>, String>;
    fn runtime_files(&self) -> Vec<GeneratedFile>;
    fn test_recipe(&self, entry: &str) -> TestRecipe;  // build cmds + run cmd
}
```

Register it in `sudoc_harness::all_backends()` and every CLI command,
harness run, and the conformance gate pick it up automatically. (An external
backend implements the same contract over JSON — the adapter implements this
trait on its behalf.)

## 2. What the IR guarantees you

By the time you see an `IrModule` (crates/ir), the frontend has already:

- resolved every name and typed every expression (`IrExpr.ty`);
- monomorphized generics into concrete functions (`sort_by__i64`);
- hoisted inout-passing calls so they only appear as the root of an
  assignment or an expression statement — your writeback story is local;
- lowered parallel assignments to temporaries;
- folded module constants to literals (overflow was a compile error);
- guaranteed match exhaustiveness, mutability rules, and that `break` /
  `continue` sit inside loops.

You are a pretty-printer plus a small runtime. If you find yourself making a
*semantic* decision, stop — either the IR already decided it, or the spec
must, and you should raise it rather than guess.

## 3. Porting order that works

Each step has corpus coverage; run `sudoc conformance --target <yours>`
continuously and watch modules flip green.

1. **Scalars + arithmetic** (`arithmetic.sudo`, `floats.sudo`): int64 with
   Overflow traps, floor div/mod, IEEE binary64. Get the trap machinery
   working here — everything else reuses it.
2. **Control flow + functions** (`loops.sudo`): if/while/for, break/continue,
   calls, inout (see §4.3), recursion.
3. **Lists and text** (`text_and_lists.sudo`, `value_semantics.sudo`): the
   first composite type; nail your copy strategy here before maps.
4. **Traps as tests** (`traps.sudo`): `expect_trap` needs a way to observe a
   trap without terminating the whole run (try/except, setjmp, catch).
5. **Maps/Sets/Options/tuples/records/enums** (`structures.sudo`): hashing
   is structural — Lists and records are valid keys; iteration order is
   yours to choose and deliberately unspecified.
6. **Hoisting semantics** (`hoisting.sudo`): mostly free if step 2 was right.
7. **Generics** (`generics.sudo`): free — you receive concrete functions and
   function pointers/references.
8. **Host boundary adapters** (lockstep.md §5): last, and optional for
   conformance; this is your language's ergonomic front door.

## 4. The land-mine catalog

Every entry below caused a real bug or near-bug in a reference backend.

### 4.1 Evaluation order
Sudo is strictly left-to-right, arguments before call, and this is
observable through traps and inout. **C's argument and operand evaluation
order is unspecified** — the C backend materializes every trapping
subexpression into a named temporary, in order, before combining. If your
language has unspecified order anywhere (initializer lists!), do the same.
Languages with fully defined left-to-right order (JS, Python, Java) can emit
expressions directly — the JS backend needed none of C's temporaries.

**Borrow/aliasing forces the same temporaries even in ordered languages.**
Rust: `put(&mut a, i, at(&a, j))` is `error[E0502]` — a shared borrow inside
a call that also takes `&mut a`. Two-phase borrows rescue *method* receivers
(`vec.push(vec.len())`) but not free-function arguments. Fix identically to
C: materialize every other operand into a temporary before constructing the
`&mut` borrow. Any language with move/borrow checking (Rust, and likely Zig
with its aliasing rules) needs this regardless of evaluation-order
guarantees.

### 4.2 Short-circuit + effects
`a or f(n) > 0` must not evaluate the right side when `a` is true — even
though the frontend hoisted `f(n)` for you, *trapping* right-hand sides you
linearize yourself must stay lazy (statement-lower them into an `if`).

### 4.3 inout
The checker guarantees inout args are variable/field paths. Pick your
mechanism: C passes pointers; Python returns `(ret?, *inouts)` tuples and
call sites reassign (`n = bump(n)`). If you use writeback-by-return,
remember void functions with inouts still return the inouts.

### 4.4 Equality
Deep, structural, with IEEE floats: `NaN != NaN` even inside a list.
**Python's list equality short-circuits on object identity**, making
`[nan] == [nan]` true — the runtime walks structures itself. Check whether
your language has the same shortcut. Map/Set equality is order-insensitive.

### 4.5 Loop lowerings
- `for i = a to b` is inclusive and evaluates bounds once. Watch the
  `b == INT64_MAX` edge: a naive `i <= b; i++` never terminates or overflows.
  The C backend computes a "was this the last iteration?" flag at the top of
  the body and lets the increment wrap through unsigned arithmetic.
- `continue` must not skip your loop's termination bookkeeping — put
  bookkeeping where `continue` cannot jump over it (loop header / update
  slot), not at the bottom of the body.
- **`break` inside `match` inside a loop**: if your `match` lowers to a
  `switch`-like construct that captures `break` (C!), you need labeled jumps
  for loop exits that cross it.
- `for x in c` iterates a snapshot; mutating `c` in the body must not affect
  the iteration (or crash your iterator — copy first).

### 4.6 Traps
- Compare by kind only; carry line/detail as diagnostics.
- A trap aborts mid-mutation. If you manage memory manually, decide what
  happens to live allocations — the C runtime threads every allocation onto
  an intrusive list and frees the lot on trap (zero leaks, verified).
- `expect_trap` nests trap observation; save and restore whatever global
  trap state you keep (the C backend memcpy's the jmp_buf).
- **Host container primitives raise their own errors, not sudo traps.** A
  host's native map/set may throw its own exception on a missing-key access
  (e.g. Haskell's `Data.Map.!`) instead of your `KeyMissing` trap — catch and
  convert at the primitive boundary, or route the surface API through your
  own trapping wrapper, so a raw host exception never leaks into
  user-visible trap comparisons.

### 4.7 Numbers
- No `-fwrapv`-style flags as semantics: make overflow checks explicit.
- `round` is ties-away-from-zero (C's `round`, NOT Python's default, NOT
  bankers'). `min`/`max` on floats: NaN if either operand is NaN;
  `min(-0.0, 0.0)` is `-0.0` (NOT C's `fmin`).
- Float division by zero is ±Inf/NaN, never a trap (Python raises — the
  runtime intercepts).
- If your language's only number is a double (JS!), i64 needs a strategy:
  BigInt, or pairs — do not silently use doubles, `9007199254740993` will
  diverge.

### 4.8 Hashing and iteration order
Map/Set iteration order is unspecified — use your language's **native** map
if it can host sudo's semantics, and prefer it when you can. Differing from
other backends is a feature (the lockstep diff catches user order-dependence),
and so is per-process randomized iteration (Rust, Swift, Go): it surfaces
order-dependent user code faster, exactly as Go intended. Divergence
verdicts involving a randomizing target may vary between runs for
order-dependent programs — the operand diagnostics in the report carry the
evidence, and the user's fix is the same either way (sort first).

The actual requirements: iteration visits each live entry exactly once;
keys compare and hash **structurally** (a `List<int>` is a valid key — in
Rust `Vec<i64>` is `Hash + Eq` natively, in Swift `[Int64]` is `Hashable`;
records derive both); map/set equality is order-insensitive; floats are
never keys (the checker guarantees it).

### 4.9 Names
- Use `sudoc_ir::names::test_fn_names` for test functions — outcome
  alignment across targets depends on it.
- Cross-module references arrive as `"module.func"` in `CallFunc`/`Const`
  names; map them to your module system (Python: real imports; C: a merged
  translation unit with `module__` prefixes).
- Reserve a prefix (`_sudo_`) for your runtime; user identifiers can't
  start with it.

### 4.10 Operator precedence is not portable
Do **not** copy another backend's precedence tiers — audit against your
language's actual grammar. The JS backend initially ported Python's table,
where `not` binds looser than comparisons; in JS `!` is unary-tier (tighter
than everything binary), so `!a === b` means `(!a) === b` — three silent
conformance divergences. JS also splits equality and relational into
*separate* tiers where Python has one. When in doubt, over-parenthesize:
redundant parens cost nothing, missing ones cost semantics.

### 4.11 User names shadow host builtins
Sudo identifiers land in your target's namespace. A user `record BigInt`
shadowed JS's global `BigInt`, breaking the backend's own Number→BigInt
coercions in that module. Reach every host builtin through an unshadowable
path (`globalThis.BigInt` in JS; fully-qualified stdlib paths elsewhere) or
route through your runtime module, whose own namespace users can't touch.
Sudo reserves no host identifiers at all, so a defensive-mangling pass is
on you even beyond the host's own reserved-word list — Haskell's Prelude
(`take`, `map`, `length`, …) collides with ordinary sudo function names
that a reserved-word check alone won't catch.

### 4.12 Test-runner traps
- Entry-point guards comparing paths must realpath both sides: macOS's
  temp dir lives under `/var/...`, a symlink to `/private/var/...` — the JS
  backend's `import.meta.url` guard silently never ran until both sides were
  realpathed.
- If your ints are unbounded (BigInt, Python), the §4.5 INT64_MAX loop
  trick is unnecessary — the increment can't wrap. Do not range-check the
  loop increment itself: `i` legitimately reaches one past the bound at exit.
- BigInt-style division truncates toward zero in most languages; sudo's
  `/` and `mod` floor. Convert explicitly.

### 4.13 Nested functions may not capture
If you lower any construct (`expect_trap`, closures) into a nested
function/struct-method, check whether your language closes over enclosing
locals. **Zig nested functions do not** — a trap block extracted into a
`struct { fn run() }` cannot see the outer scope's variables at all
(threading them as params hits shadowing and "crosses namespace boundary").
The fix that generalizes: lower such blocks *inline* (Zig: a labeled block is
an ordinary scope — rewrite `try X` to `X catch |e| break :lbl e` and yield
an optional error). Conformance won't catch this if your corpus blocks
self-contain their locals — only the full harness suite will. Run
`cargo test --workspace`, not just `sudoc conformance`, before claiming done.

### 4.14 Traps your language can't catch
Some trap kinds may be unobservable in a target: Rust and C cannot catch a
real `StackOverflow` (the process aborts), Swift's native integer overflow
is an uncatchable `fatalError` (so route ALL int arithmetic through throwing
helpers — never emit a bare `+`/`-`/`*` on the int type). Map each sudo trap
to a *catchable* mechanism; where none exists (StackOverflow), document it as
a known soft spot — the current corpus doesn't exercise it. Never emit an
operation whose failure mode is an uncatchable abort when the spec says trap.

### 4.15 Readable output
Generated code is a build artifact, but readability is a product goal: keep
user identifiers, real control flow, and comments to a minimum of mangling.
When in doubt, ask "could a reviewer debug this?"

### 4.16 Host bindings may be recursive
Sequential rebinding of a source name through the host's binding form can
be self-referential. Haskell's `let !n = n + 1` is a black hole: `let` is
recursive, so the bang-patterned RHS sees the *new* `n`, not the old one,
and it loops forever (`<<loop>>`) instead of raising anything catchable.
Fix: use a non-recursive binding form for statement-level rebinds —
Haskell's `case e of { !n -> ... }` binds `n` non-recursively, so the RHS
still sees the outer `n`. Generalize before porting: check whether your
host's binding construct is recursive before you reuse a sudo variable
name across a rebinding; if it is, reach for the non-recursive form your
language offers instead.

### 4.17 Lazy hosts defer traps out of their test
In a lazy language, a trap raised while building an unevaluated thunk
doesn't fire where it was produced — it fires wherever something
eventually forces that thunk, which may be a later test, or none at all if
the value is never demanded. A TAP runner that just wraps each test body
in try/catch is not enough: the trap silently escapes to a later test's
`try` block (misattributing the failure) or vanishes entirely. Fix: force
each test's result to normal form *inside* that test's own try/catch
boundary before moving on — Haskell: try + evaluate + a deep-force helper
(not a bare `try (evaluate result)` that only reaches WHNF).

### 4.18 In pure hosts, mutation is loop state — all of it
When loops compile to a recursive function threading the variables a loop
body mutates, "mutates" must include every route to mutation, not just
plain `x = ...` assignment: the receiver roots of mutating-builtin
statements (`items.swap(...)`, `items.append(...)`) and inout writeback
targets have to be in the threaded parameter set too. Miss one and the
symptom is silent, not a compile error: `sort_by` (or any loop that
mutates only via a builtin call) returns its input unchanged, because the
"mutated" list never actually flows through the loop helper's recursion.
Related trap: mutating builtins can nest inside otherwise-pure expressions
(`assert a.pop() == 1`), so a stateless host needs a hoist pass that pulls
each mutating builtin call out into its own bind before the pure
expression is lowered — statement-level handling alone misses these.

### 4.19 Manual-memory backends should be sanitizer-clean
If your target compiles to native code with manual memory management (C,
Zig, Rust `unsafe` blocks, …), instrument test builds with your toolchain's
memory/UB sanitizers where available and wire that into your `test_recipe`
by default — with a graceful fallback when the toolchain lacks support, and
an opt-out for when instrumentation itself is the obstacle. The C backend is
the reference: `sudoc_backend_c::sanitize_status()` probes once per process,
`SUDOC_NO_SANITIZE=1`/`--no-sanitize` opts out. A sanitizer hit during
`sudoc test`/`conformance` is a backend bug — the corpus is expected to run
clean under instrumentation — and should be surfaced distinctly from an
ordinary runner crash or a test-assertion trap.

## 5. The runtime you'll write

Every backend grew a small runtime with the same inventory — budget for:
checked i64 arithmetic + floor div/mod, IEEE float helpers (min/max/round),
bounds-checked list ops, a structural-key hash map/set, deep copy + deep
equality, trap raise/observe machinery, canonical serialization for assert
diagnostics (spec lockstep.md §4), text codecs for the host boundary, and
the test runner loop. Reference implementations: `_sudo_rt.py` (~350 lines),
`sudo_rt.h/c` (~300 lines).

## 6. Future: out-of-process backends

Once the IR schema stabilizes, a JSON-over-stdio protocol will let backends
be written in any language as external executables, slotting in as one more
`Backend` implementation. The seams are already process-shaped (files in,
TAP out); nothing you build against the trait today will be invalidated.
