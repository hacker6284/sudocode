# Writing a sudo Backend

The handbook for adding a target language. Everything here was learned the
hard way while building the Python and C reference backends; follow the
porting order and the land-mine catalog and you will spend your time on your
language's idioms instead of rediscovering semantics bugs.

**Definition of done**: `sudoc conformance --target <yours>` is green — every
module in `conformance/semantics/` agrees with the reference backends under
the lockstep harness. That corpus *is* the semantics of sudo, executable.

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
    fn name(&self) -> &'static str { "js" }
    fn emit_program(&self, modules: &[IrModule], with_tests: bool) -> Vec<GeneratedFile>;
    fn runtime_files(&self) -> Vec<GeneratedFile>;
    fn test_recipe(&self, entry: &str) -> TestRecipe;  // build cmds + run cmd
}
```

Register it in `sudoc_harness::all_backends()` and every CLI command,
harness run, and the conformance gate pick it up automatically.

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
Map/Set iteration order is unspecified — you may use whatever is natural,
and *differing* from other backends is a feature (it catches user bugs).
But your order must be deterministic run-to-run. Keys hash structurally;
floats are never keys.

### 4.9 Names
- Use `sudoc_ir::names::test_fn_names` for test functions — outcome
  alignment across targets depends on it.
- Cross-module references arrive as `"module.func"` in `CallFunc`/`Const`
  names; map them to your module system (Python: real imports; C: a merged
  translation unit with `module__` prefixes).
- Reserve a prefix (`_sudo_`) for your runtime; user identifiers can't
  start with it.

### 4.10 Readable output
Generated code is a build artifact, but readability is a product goal: keep
user identifiers, real control flow, and comments to a minimum of mangling.
When in doubt, ask "could a reviewer debug this?"

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
