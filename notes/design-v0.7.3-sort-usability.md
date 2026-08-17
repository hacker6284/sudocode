# v0.7.3 — make copy-on-write uniform

Status: implemented 2026-08-17. `dup` of a list/map/set/record is an
unconditional O(1) share. Nested writes go through `at_mut` / `field_mut`
/ `map_at_mut`. Forking a parent `_uniq`s by sharing each child, so a
later child write cannot leak. Unpack elision (Fix B) and gated counters
(Fix C) ship in this tag. Fix A (scan) was cut. Host-facing records
unwrap via `out_record`. No API, protocol, or language change. 0.7.2
was not tagged.

Acceptance (py, n=4000, best of 3):

- `sort_by` 0.028s vs 3× `sort_by_key` 0.215s (7.7× faster).
- E3 vs E1 is 1.0×.
- Width 256 vs 1: tuples 0.85×, `List<text>` 0.82×, `List<Row>` 0.91×.
- `copy_texts` 512 vs 1 is 1.01×; vs `copy_ints` is 3.2×.
- Records are COW handles (`CowRec`): `dup` is share, field writes fork.
- Checksums agree (596013942).

The originating requirement is preserved below, from infinite-craft-cli.

---

# Spec for sudocode 0.7.3 — make copy-on-write uniform

## Where this sits

- **0.7.1 (shipped).** Swap-as-rebinding + conditional COW. Verified: whole-list
  dups per `sort_by` are 2, flat in n. `infinite-craft-cli` deleted its
  hand-rolled heapsort on the strength of it — 97,020 pairs, byte-identical
  ordering, 40.5s → 26.1s. That was the goal and it landed.
- **0.7.2 (in flight).** Unpack elision + instrumentation gating, from the
  previous spec. Delivers the larger share of the remaining sort win.
- **0.7.3 (this document).** Replace conditional sharing with uniform
  copy-on-write, so the fix stops being shaped like one consumer's data.

## Why this exists

0.7.1's COW shares a list's backing array only when every element is
"alias-safe" — scalars and tuples. A `text` *is* a `CowList` and a record *is*
a dataclass, so `List<text>`, `List<Element>` and every nested container are
excluded and still deep-copy element by element.

The one shape that does qualify — a tuple of scalars and texts — is precisely
`infinite-craft-cli`'s decorated sort row. That is not a general fix; it is a
fix for the benchmark I supplied. This document is me correcting for that,
having specced the narrow version myself in the previous draft.

## Note for the in-flight 0.7.2

0.7.3 **deletes `_elems_alias_safe`**. If 0.7.2 still contains the narrow
Fix A — memoizing that scan — consider cutting it:

- It is the correctness-sensitive part of 0.7.2. A memoized alias-safety answer
  needs invalidating whenever an element of an unsafe type is stored into the
  list; get that wrong and a mutation silently leaks through a shared referent.
  That is a real hazard to take on for code with a one-release lifespan.
- It is the smaller half of the win. Measured on the shipped 0.7.1 runtime,
  n = 4,000, `List<(int, text, text, int)>`, E3 composite sort-only:
  stock 0.465s → memoized scan 0.193s (2.4×) → unpack elision 0.022s (8.8×).
- Uniform COW subsumes it completely, and does so for every container type
  rather than just tuples.

0.7.2 as Fix B + Fix C is a clean, low-risk release that still takes the
composite sort most of the way. Nothing below depends on the narrow Fix A
having shipped.

## The change

Invert the default. Today it is *deep-copy unless sharing can be proven safe*,
and the proof is O(n) on every `dup`. Make every composite — list, `text`,
record, map, set — a copy-on-write handle:

- `dup` is unconditionally O(1) for every type. No scan, no element-type
  special cases, no memoized cache to invalidate.
- A mutating operation forks the specific object it writes, if and only if that
  object has a second referent.
- `_elems_alias_safe` is deleted. There is nothing left to prove: a write
  through *any* referent forks before writing, so no mutation is ever visible
  through another handle. Same invariant the scan was defending, enforced at
  the write instead of guessed at the copy — and it is less code than the rule
  it replaces.

## Evidence

n = 4,000, the same text payload in three containers, py, sort-only time on the
shipped 0.7.1 binary:

| container | w=8 | w=64 | w=256 | 256/8 | qualifies for sharing? |
|---|---|---|---|---|---|
| `List<(int,text)>` tuple | 0.268s | 1.162s | 4.264s | **15.9×** | yes |
| `List<text>` | 0.224s | 0.779s | 2.654s | **11.8×** | no |
| `List<Row>` record | 0.197s | 0.755s | 2.643s | **13.4×** | no |

Every container still scales with payload, and the qualifying one is the
slowest of the three — the linear proof costs more than the copy it avoids.

Absolute times are not directly comparable here: the three comparators differ,
and `row_rec_less` reads a field without unpacking, which is why records look
best. The comparable figure is the scaling ratio, and none of them is
payload-independent.

After 0.7.2's unpack elision, the tuple row improves substantially. The other
two do not — unpack elision touches destructuring, while their cost is
`sort_by` duplicating elements internally. Only uniform COW reaches them.

### Correction: RC3 did not dissolve

The 0.7.1 notes say:

> *"Side effect: `dup(text)` is O(1) too, so the 0.7.2 'text is List<int>,
> copying one is O(length)' item dissolves on py/js. Confirm with `copy_texts`
> in `sortbench.sudo` before spending a release on it."*

I confirmed with `copy_texts`, as instructed, and it does **not** hold: 229×
across widths on the shipped binary. The copy is O(1); the alias scan that
guards it is not, and it is on the same path. The note should be corrected —
but its *conclusion* becomes true once uniform COW lands, at which point RC3
really does dissolve and no separate text-representation work is needed — a
`text` becomes a COW handle like everything else, so duplicating one is O(1)
whatever its length. That is the good outcome; it just does not follow from
0.7.1 alone.

---

## Acceptance criteria for 0.7.3

Numbering carried from the 0.7.1 spec. "0.7.2 expected" is what unpack elision
alone should achieve; 0.7.3 must hold the ones it already passes and clear the
rest.

| # | criterion | 0.7.1 measured | 0.7.2 expected | 0.7.3 required |
|---|---|---|---|---|
| 1 | `sort_by` beats the radix decomposition | 0.458s vs 0.354s — FAIL | PASS | hold |
| 2 | composite penalty ≤ 3× vs `List<int>` | 17.1× — FAIL | ~1.0× PASS | hold |
| 3 | payload independence ≤ 2×, **tuple** element | 30.7× — FAIL | PASS | hold |
| **3b** | **payload independence ≤ 2×, `List<text>`** | **11.8× — FAIL** | still FAIL | **PASS** |
| **3c** | **payload independence ≤ 2×, `List<record>`** | **13.4× — FAIL** | still FAIL | **PASS** |
| **3d** | **`dup` cost independent of element type** — no container class is on a deep-copy path | FAIL by construction | still FAIL | **PASS** |
| 4 | whole-list dups O(1), not O(log n) | 2, flat — **PASS** | hold | hold |
| 5 | copy volume bounded | restated below | | |
| 6 | no API change | **PASS** | hold | hold |
| 7 | op-count gate against regression | **PASS** (`//stdlib:sorting_sort_copy_complexity`) | extend | extend to 3b/3c |

3b, 3c and 3d are the point of this release. Criteria 1–3 can all be satisfied
by optimising tuples again, which is how 0.7.1 ended up shaped like one
consumer's row type; 3d is the one that forbids that.

Cases `sort_texts` / `sort_records` in `sortbench.sudo` (E7) measure 3b and 3c.

**Criterion 5 needs restating.** It was written as "scalars copied ≤ 4× the
list's own scalar count", which assumed copying was the cost. Under COW the
copy is O(1) and the cost is the *scan*, which the copy counter does not see —
`dup_stats()` reported healthy numbers while the payload probe still showed
30×. Replace it with: **work per `dup` is independent of the duplicated
value's payload size**, measured as the width-256-vs-width-1 ratio in criterion
3 rather than as a copy count. A metric that goes green while the program is
still quadratic in payload is worse than no metric.

**Extend criterion 7 to cover Fix B**: a doubling test asserting that unpack
count, not unpack *cost*, scales with comparisons — i.e. that reading a tuple
field does not scale with the size of fields it does not read.

## Non-goals

- No API change. `sort_by(items, less)` keeps its signature. Nothing in
  `craft.sudo` should change to benefit — it already calls `sort_by` directly.
- Do not touch the `text`-as-`List<int>` representation. Under uniform COW a
  `text` is a handle like any other composite, so its representation stops
  mattering; changing it would be a much larger release for no measured gain.
- Do not weaken value semantics anywhere. Uniform COW must be *observationally
  identical* to deep copying — the fork happens before the write, never after.
  This is the whole correctness risk of the release.
- Do not remove the stability contract or the radix recipe from
  `stdlib/README.md` — it stays legal and useful, it just stops being
  mandatory for anyone who wants a multi-key sort to be fast.

## Reproducing

```
sudoc build --target py -o /tmp/sb bench/sortbench.sudo
python3 bench/sort_run.py /tmp/sb 4000 3
```
