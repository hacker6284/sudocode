# Zig backend: per-frame scratch reclamation (design, not yet built)

Status: **designed + advisor-vetted, deliberately deferred.** The borrow fix
(dc813a6) already cut the kernel from 26.6 GB to 1.8 GB and the regex from
3.7 GB to 21 MB. This note captures the remaining reclamation so it can be
executed deliberately rather than one-shot. See decision-log 2026-07-23 for
why it was deferred (footprint-only win vs. a correctness-sensitive change to
the core allocation path, with a safety net that only works if we build it).

## What the residual is

The Zig runtime uses ONE global arena, reset only *between tests*. Everything a
hot loop allocates and drops within an iteration — `sorted(recipes.keys())`
snapshots, `new_pairs` maps, `recipes[k]` list copies — stays live until the
end-of-test reset. Python (25.6 MB) and C (~3 MB) on the same source do not
blow up: the BFS mutates `parent`/`visited` in place and its working set is
MB-scale. So the 1.8 GB is purely the never-free arena holding dead
per-iteration transients, not an algorithmic flaw.

## The design (three parts)

### 1. Two arenas + a global active pointer  (sudo_rt.zig)

- Keep `sudo_arena` (PERSISTENT). Add `sudo_scratch` (a second ArenaAllocator).
- `var active: *std.heap.ArenaAllocator = &sudo_arena;` — persistent is the
  default. `allocator()` returns `active.allocator()`. Every existing
  allocation site (`SudoList.append`, `SudoMap.put`, `box`, `key_dup`) already
  funnels through `allocator()`, so no container API changes.
- Scratch is **opt-in per statement**, never the ambient default. A
  scratch-routed statement emits `const _p = rt.pushScratch(); defer rt.pop(_p);`
  around its RHS evaluation. Default-persistent means the fallback is always the
  current, already-correct behavior — the safe direction.

### 2. Dynamic loop claim + reset  (codegen: While / ForRange / ForIn)

- One scratch arena, claimed by the **dynamically outermost** active loop. Loop
  prologue: `const _claim = rt.scratchClaim();` (increments a depth counter;
  returns true iff depth went 0→1). Per-iteration boundary: `if (_claim)
  rt.scratchReset();`. Loop epilogue: `rt.scratchRelease(_claim);`.
- Only the claimant resets, so nested loops — including loops inside functions
  *called* from the loop body — share one frame and never free a live
  outer-iteration value. Memory is bounded to one outer-iteration's transients
  (~6 MB on the kernel), not per-inner-iteration optimal, but sound. Document
  that a hot inner loop inside a callee accumulates within one outer iteration.

### 3. Conservative escape analysis  (codegen: new static pass)

This is the crux and the risk. A value may be routed to scratch ONLY if it
provably cannot outlive the current loop iteration. Route a statement's RHS to
scratch iff its destination root is a **non-escaping loop-inner local**;
otherwise leave it persistent. Callees inherit the ambient allocator — there is
**no** callee-side forcing (a callee returns in ambient, so `keys =
sorted_text_list(recipes.keys())` with `keys` transient builds the result in
scratch; the same call bound to a persistent local builds it persistent).

Per function, compute `escaping: HashSet<String>` as a fixpoint (model on
`collect_written` / `written_expr`):

- **Seed:** any local that appears in a `Return` expr; any local stored into a
  param, an `inout` target, a module const, or a local declared *outside* the
  loop it is written in (loop-carried state like `parent`/`visited`); any local
  passed as an `inout` argument.
- **Propagate:** if an escaping local's defining RHS aliasing-reads local X
  (per `aliasing()` at lib.rs:2661), X escapes too. Iterate to fixpoint.
- **Scratch-eligible** = first-declared (`declares=true`) lexically inside a
  loop AND ∉ `escaping`. Route an Assign / mutating-builtin statement to scratch
  iff its target root is scratch-eligible. Everything else stays persistent.

The kernel's big transients (`keys`, `new_pairs`, `pairs = recipes[k]`) are
loop-inner and never escape → scratch. `parent`/`visited` are declared before
the loop and mutated in place → persistent. Returns and inout writes →
persistent. Default when unsure → persistent (sound).

## The safety net is NOT free — build it first  (the deferral's core reason)

`reset(.retain_capacity)` rewinds **without poisoning**: a stale pointer reads
valid old bytes and a use-after-reset can pass lockstep for many iterations
before capacity is reused. Do **not** rely on "lockstep self-checks it." In
ReleaseSafe/debug builds, `@memset` the rewound scratch region on every
`scratchReset()` (or `reset(.free_all)` periodically) so any mis-scoped value
reads poison and diverges loudly. This poisoned build is the acceptance oracle,
not an afterthought.

## Execution plan (layered, each layer independently green)

1. **Layer 1 — runtime plumbing only.** Add `sudo_scratch`, `active`, the
   claim/reset/release API, and poison-on-reset. No codegen change yet: nothing
   uses scratch, so the whole suite must stay byte-identical green. Clean
   rollback point.
2. **Layer 2 — codegen.** Loop claim/reset wrapping + the escape pass + per-
   statement scratch routing. Verify against the **poisoned** build.

## Acceptance bar (hard)

- Kernel peak RSS to MB-scale (target ~single-digit MB, from 1.8 GB).
- Full seven-target suite green (conformance 11/11, kernel 18/18, examples +
  stdlib) — AND green under the **poisoned** scratch build, which would catch a
  mis-scoped reset.
- If the poisoned build cannot be made confidently green, land Layer 1 +
  whatever Layer 2 subset is proven and document the residual — do NOT ship a
  silent use-after-reset into the core allocator.

## Why this is not overfitting

Route-by-escape + dynamic-claim scratch + poison-on-reset is the *general*
per-frame arena-reset pattern (idiomatic Zig), correct for any hot loop with
transients. Nothing keys off the kernel's specific builtins or shapes.
