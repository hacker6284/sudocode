# Zig backend: honest scoped-allocator overhaul (design + staging)

Status: **architecture vetted, staged, in progress.** Supersedes this doc's
earlier footprint-only two-arena scratch design. Owner reframed the defect: the
Zig memory model is DISHONEST and non-idiomatic (a hidden process-global arena
reached from inside container ops, freed only between tests) — footprint is a
symptom, not the driver. Full overhaul greenlit; form delegated to the
architect. Decision + rationale: notes/decision-log.md (2026-07-23 "OVERHAUL").

## The principle

Create every value in an in-scope arena; at each **store boundary** deep-copy it
into the **destination's own allocator**. This IS sudo value semantics — `store()`
already deep-copies at these points; today the copies just target a global sink
that never drains. The boundary set is **provably complete** because sudo has no
observable aliasing (spec §2.1): every place-expression roots at a named
local/param, so there is no fourth escape channel — hence NO whole-program
fixpoint is needed, Tier 2 included.

Store boundaries: `return`, `inout` write/replace, store-into-container,
assign-to-outer-scope-local, and (Tier 2) the **loop-iteration boundary** —
a value surviving to the next iteration is by definition one written to
loop-carried storage, which is already a copy-into-longer-lived-allocator.

## Architecture (advisor-vetted)

- **Containers carry their allocator** (managed): `SudoList/Map/Set` gain an
  `alloc` field set at creation; mutating ops use `self.alloc`; global
  `allocator()` removed. Mutation-through-inout then lands in the caller's
  allocator for free.
- **inout whole-value replacement** of an allocator-less composite (`p := newrec`
  where the old value has no container to read `.alloc` from, and `ret_alloc` is
  wrong for a forwarded inout) → thread ONE `alloc` param per `needs_dup` inout
  param, used only at replacement boundaries. Managed containers everywhere else.
- **Functions take a uniform `ret_alloc: std.mem.Allocator` first param.** Uniform
  (not just allocating functions) because fn-pointer types / `FuncRef` /
  `CallValue` / address-taken funcs must share one signature — it is all-or-none.
  Non-allocating functions ignore it (`_ = ret_alloc;`) and skip the arena.
  Allocating functions open `var scratch = ArenaAllocator.init(ret_alloc); defer
  scratch.deinit(); const sa = scratch.allocator();`; internals default to `sa`;
  callees receive the ambient scratch as their `ret_alloc`.
- **`store()` targets the destination allocator** and CLOSES its copy-elision for
  non-aliasing RHS at an escaping destination (else use-after-free once the
  source is a shorter-lived scratch).
- **Tests** create a per-test `ArenaAllocator` in-body (the `TestCase.func`
  pointer is a fixed zero-arg sig — the allocator cannot be a param) via `defer`,
  and thread it down as `ret_alloc`. This replaces the between-tests global
  reset. Traps free via `defer` (cleaner than C's intrusive free-list).
- **Module const-init** (`sudoInitConsts`) takes an allocator param and builds
  consts into the current test's arena (rebuilt per test, as today).

## Tier 2 (hot loops) — folds into the same rule

Arena per loop LEVEL (a stack keyed by loop depth), reset per iteration. Loop
body ambient = innermost loop arena; **calls inside the loop receive that arena
as `ret_alloc`** (else a call-heavy loop like the BFS `compute_layers` gets no
benefit). Cross-iteration / cross-depth assigns copy into the declaring scope's
arena. **for-in snapshots live in the ENCLOSING arena** (they span the whole
loop, so must not sit in the per-iteration arena). Needs only light lexical
depth tracking, not dataflow. Relies on block scoping (loop-inner locals
unreadable after the loop) — already enforced by Zig's block emission.

## The acceptance oracle

The global arena has been MASKING every latent lifetime bug; post-overhaul a
single shallow copy or one missed elision-closure is SILENT corruption under
ReleaseSafe. Mitigation: run the full seven-target suite in **Debug backed by
`std.heap.DebugAllocator`**, which traps real use-after-free. This is the bar
for Stages 2+, not an afterthought. (Add via the test recipe at lib.rs ~3735.)

## Staging (each stage compiles + passes; keeps the seam)

1. **Stage 1 — mechanical, behavior-preserving (dispatched).** Containers gain
   an `alloc` field; `box`/`copy_*`/mutating ops take an explicit allocator; a
   single codegen seam `Emitter::cur_alloc()` returns the allocator expression,
   hard-coded to `"rt.allocator()"` so behavior is identical (kernel stays
   ~1.8 GB — expected). Proves managed containers compile in Zig; isolates
   threading from lifetime change. No signature/fn-pointer/test changes.
2. **Stage 2 — semantic core.** Flip `cur_alloc()` to the scoped allocator; add
   the uniform `ret_alloc` param + `zig_ty(Ty::Func)` update + per-function
   scratch arena; make `store()` boundary-aware (destination allocator) and
   close the elision hole; thread the per-inout replacement allocator. Stand up
   the DebugAllocator oracle and verify under it.
3. **Stage 3 — per-test arena; delete the global reset and the global arena.**
4. **Stage 4 — Tier 2:** per-loop arenas + the iteration boundary + for-in
   snapshot placement.

## Hardest threading constraints (from the touchpoint inventory)

- `TestCase.func: *const fn () SudoError!void` — fixed zero-arg; per-test
  allocator must be created inside the test body, not passed in. Pivot of Stage 3.
- `Ty::Func` fn-pointer type + `FuncRef`/`CallValue`/address-taken — `ret_alloc`
  is all-functions-or-none (Stage 2), cannot be piecemeal.
- `keyapp_*` is bound into `SudoMap(...,appendKey: fn(K) void)` — fixed sig, but
  touches only the static key buffer, so no allocator needed there.
- `box` is comptime-generic with ~9 emission sites; `copy_*`/`eq_*`/`canon_*`
  live in the shared `sudo_types.zig` (imports only `rt`) with cross-module
  `pub const` aliases — an allocator param there ripples to every bare-name caller.
- `store()` is the sole copy chokepoint (~25 call sites) but doesn't know WHICH
  boundary it's at today — per-boundary allocator selection pushes down through
  those sites in Stage 2.

## Not overfitting

The whole mechanism is the standard idiomatic Zig arena discipline (scoped
arenas, `defer` deinit, explicit allocators, copy-on-store for value semantics).
Nothing keys off any benchmark's specific shapes.
