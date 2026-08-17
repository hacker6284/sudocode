# v0.7.2 — finish the job 0.7.1 started

Status: **not released.** Implemented locally, then superseded by 0.7.3
before a tag. Fix B (unpack elision) and Fix C (gated counters) shipped
in 0.7.3. Fix A (memoized alias-safety scan) was cut: 0.7.3 deletes
`_elems_alias_safe` and makes every composite a COW handle instead.
See `notes/design-v0.7.3-sort-usability.md`.

The originating requirement is preserved below, from infinite-craft-cli,
verified against published 0.7.1
(`f2317d547abc2887b88822b6309f9cb62aa7ec70310969b41a2b56622364baa0`).

Acceptance (py, n=4000, best of 3 on the implementer's machine;
growth gated in CI, wall-clock is the bench):

- `sort_by` 0.039s vs 3× `sort_by_key` 0.261s (6.7× faster).
- E3 vs E1 is 1.0× (0.039s / 0.039s).
- Width 256 vs 1 is 0.83×. `copy_texts` 512 vs 1 is 1.03×.
- Whole-list dups stay 1 as n doubles; scan stays 0.
- Checksums agree (`sort_rows_checksum` == `sort_rows_bykey_checksum`
  = 596013942).
- Unpack-then-mutate still isolated (`never_written_elision.sudo`,
  `value_semantics.sudo`).
- No API change.

---

# Spec for sudocode 0.7.2 — finish the job 0.7.1 started

## Status: 0.7.1 worked. Here is what it did and what is left.

0.7.1 delivered both root causes from the 0.7.1 spec. Verified independently
against the published binary
(`f2317d547abc2887b88822b6309f9cb62aa7ec70310969b41a2b56622364baa0`), not taken
from the release notes:

- **RC1 fixed.** `items, buf = buf, items` emits as a rebinding. Whole-list
  dups per `sort_by` are now **2, flat in n** (were `4·log₂(n)+2` — 42/46/50/54
  at n = 1,000/2,000/4,000/8,000). Acceptance criterion 4: **PASS**.
- **RC2 fixed.** COW lists; tuple slot stores share.
- **Consumer impact, the thing that actually mattered:** `infinite-craft-cli`
  has **deleted its hand-rolled heapsort**. On the real kernel, 441 choose-2 =
  97,020 pairs, py, byte-identical output ordering: heapsort **40.5s**, one
  `sorting.sort_by` call **26.1s**. The builtin now beats the bespoke code, so
  the bespoke code is gone. That was the entire point. Thank you.

What is left is that `sort_by` on composite elements is still **17×** slower
than on `int`s at the same comparison count, and still **loses to the 3×
`sort_by_key` radix decomposition** in isolation. Two remaining causes, both
measured, both with a simulated fix and a predicted result.

**Fixing both takes composite sorting to parity with `int` sorting — a 21×
improvement on the composite case, and `sort_by` then beats the radix
workaround by 8.5×.** That is the ferrari.

## Headline: what the two fixes are worth

n = 4,000, `List<(int, text, text, int)>`, py, sort-only time, checksum-verified
identical ordering at every step:

| | E1 `List<int>` | E3 composite<br>(comparator reads 1 int) | ratio | E5 radix | verdict |
|---|---|---|---|---|---|
| 0.7.1 as shipped | 0.027s | 0.465s | **17.1×** | 0.354s | `sort_by` loses |
| + Fix A (alias scan) | 0.027s | 0.193s | 7.2× | 0.264s | `sort_by` wins |
| + Fix B (unpack) | 0.022s | **0.022s** | **1.0×** | 0.203s | `sort_by` wins **8.5×** |

Both simulations were applied to the shipped runtime and validated by a
positional checksum of the sorted order (`sort_rows_checksum`), which is
identical across all three rows.

---

## Fix A — the alias-safety check is O(n) on every `dup`

COW made the *copy* O(1) and then put a linear scan on the same path:

```python
def _elems_alias_safe(xs) -> bool:
    for x in xs:                                       # every element…
        if isinstance(x, (CowList, SudoMap, SudoSet)): # …every dup
            return False
        if is_dataclass(x):
            return False
    return True
```

`dup` calls this before `share()`. So duplicating a `text` (a `CowList` of
codepoints) is still O(length) — the cost moved from copying to scanning, and
the asymptotics did not change.

**Evidence.** `copy_texts(20000, width)` — append one text into a `List<text>`
20,000 times, no sort involved:

| text width | 0.7.1 | with Fix A |
|---|---|---|
| 1 | 0.015s | 0.007s |
| 128 | 0.551s | 0.008s |
| 512 | 3.394s | **0.008s** |
| **512 ÷ 1** | **229×** | **1.1×** |

And in the sort, the E4 payload probe (comparator reads only the int, never
touches the text): **30.7× → 0.99×** at width 256 vs width 1.

**Fix.** The compiler knows each list's static element type. `List<int>` — and
therefore every `text` — is alias-safe by construction and never needs the
scan. Carry it as a flag on the `CowList` set at construction from static type
info; failing that, memoize per backing box and invalidate when an element of
an unsafe type is stored. The simulation above memoizes per box.

**Fixes acceptance criteria 1 and 3.**

### Correction: RC3 did not dissolve

The 0.7.1 notes say:

> *"Side effect: `dup(text)` is O(1) too, so the 0.7.2 'text is List<int>,
> copying one is O(length)' item dissolves on py/js. Confirm with `copy_texts`
> in `sortbench.sudo` before spending a release on it."*

I confirmed with `copy_texts`, as instructed, and it does **not** hold: 229×
across widths on the shipped binary. The copy is O(1); the alias scan that
guards it is not, and it is on the same path. The note should be corrected —
but its *conclusion* becomes true once Fix A lands, at which point RC3 really
does dissolve (1.1×) and no separate text-representation work is needed. That
is the good outcome; it just needs Fix A first.

---

## Fix B — destructuring copies the source even when nothing is written

The generated comparator, for a function that reads exactly one field:

```python
def row_less_int_only(a, b):
    a_s, a_k1, a_k2, a_i = _rt.dup(a)     # full tuple copy
    b_s, b_k1, b_k2, b_i = _rt.dup(b)     # full tuple copy
    return a_s < b_s
```

Neither `a` nor any binding drawn from it is ever written. This is exactly the
`never_written` analysis 0.6 built for parameters — it simply is not applied to
destructuring. Note the parameter itself is already handled correctly (there is
no copy at the *call*); the copy is at the *unpack*.

This costs 2 tuple dups per comparison × n log n comparisons, and it is the
entire residual after Fix A: it is why criterion 2 sits at 7.2× rather than 1×.

**Fix.** Extend `never_written` to unpack bindings: if no binding from a
destructure is assigned, index-mutated, field-mutated, passed as `inout`, or
used as a mutating-builtin receiver, unpack the source directly without `dup`.
Keep the copy when any binding is written — `a, s = ys[0]; s.append(…)` must
still not leak, which is the behaviour the 0.7.1 notes correctly call out.

**Fixes acceptance criterion 2** (17.1× → 1.0×).

This one is not sort-specific and is worth more than the sort case: every
tuple-destructuring read path in every sudo program pays it today.

---

## Fix C — the dup instrumentation is unconditionally live

```python
def _count_list_dup(n: int) -> None:
    _DUP_STATS["list"] += 1
    d = _DUP_STATS["list_by_len"]
    d[n] = d.get(n, 0) + 1
```

Called from `dup` on every list duplication, in the shipped runtime, with no
env gate on the call itself. Measured cost: **~10%** on `sort_by`
(0.498s → 0.449s with it stubbed out).

It is genuinely useful — `dup_stats()` is what let me verify criterion 4 from
outside — so keep the capability, but gate the counting so programs that are
not measuring do not pay. Backend guide §4.20 already requires that
instrumentation never change what a program computes; this is the performance
half of that same principle.

Lowest priority of the three. It is 10%, not 21×.

---

## Acceptance criteria for 0.7.2

Carrying forward the numbering from the 0.7.1 spec, with measured current
values and simulated post-fix values:

| # | criterion | 0.7.1 | predicted | |
|---|---|---|---|---|
| 1 | `sort_by` beats the radix decomposition | 0.458s vs 0.354s — FAIL | 0.024s vs 0.203s | **PASS** |
| 2 | composite penalty ≤ 3× vs `List<int>` | 17.1× — FAIL | 1.0× | **PASS** |
| 3 | payload independence ≤ 2× (width 256 vs 1) | 30.7× — FAIL | 0.99× | **PASS** |
| 4 | whole-list dups O(1), not O(log n) | 2, flat — **PASS** | 2 | hold |
| 5 | total copy volume bounded | needs restating under COW (see below) | | |
| 6 | no API change | **PASS** | **PASS** | |
| 7 | op-count gate against regression | **PASS** (`//stdlib:sorting_sort_copy_complexity`) | extend | |

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
- Do not touch the `text`-as-`List<int>` representation. Fix A makes it
  irrelevant on py/js; changing it would be a much larger release for no
  additional measured gain.
- Do not weaken the copy-on-unpack rule where a binding *is* written. Fix B is
  a `never_written` refinement, not a removal.
- Do not remove the stability contract or the radix recipe from
  `stdlib/README.md` — it stays legal and useful, it just stops being
  mandatory for anyone who wants a multi-key sort to be fast.

## Reproducing

Harness is the one now in upstream `bench/` (`sortbench.sudo`, `sort_run.py`),
plus two additions I would land alongside:

- `sort_rows_checksum` / `sort_rows_bykey_checksum` — positional checksums of
  the sorted order, so a runtime experiment that silently corrupts ordering
  fails loudly. My first attempt at simulating a fix *did* corrupt the order;
  the checksum is the only reason I did not report a bogus number.
- `copy_texts(n, width)` / `copy_ints(n, width)` — the RC3 probe, the thing
  that shows Fix A is still needed.

```
sudoc build --target py -o /tmp/sb bench/sortbench.sudo
python3 bench/sort_run.py /tmp/sb 4000 3
```
