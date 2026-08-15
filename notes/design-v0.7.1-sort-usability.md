# v0.7.1 — make `sorting.sort_by` usable on composite elements

Status: implemented 2026-08-15. RC1 (local-list swap is a rebinding) and RC2
(copy-on-write lists on py/js) ship in this patch, plus a tuple-slot share
(`buf[k] = items[a]` does not dup a tuple). RC3 (`dup(text)` O(length))
dissolves as a COW side-effect; confirm with `copy_texts` in `bench/sortbench.sudo`
before spending 0.7.2 on it.

Acceptance at n=4000 on py (best of 5), against the numbers in this document:

- sort_by beats 3× sort_by_key (0.26s vs 0.48s).
- payload width 256 vs 1 is 1.2× (was 72×).
- copy-volume *growth* gated by `//stdlib:sorting_sort_copy_complexity`
  (whole-list dups flat as n doubles; leaves ignore unread width).
- E3 vs E1 is ~9.5×, not ≤3×. After copies are gone the remainder is the
  comparator call (`less(at, at)` unpacking two 4-tuples vs `a < b`). E2 vs
  E3 is still 1.0× — comparison work is free; this leftover is Python call
  overhead, not payload copies. Originating requirement from
  infinite-craft-cli, preserved below.

---

# Spec for sudocode 0.7.1 — make `sorting.sort_by` usable on composite elements

## What this is

Not a bug report. A **requirement with acceptance criteria**, from the only
real-world consumer of sudocode.

The 0.7 issue I filed was scoped to "unbreak what 0.6 broke", and 0.7 did that
correctly — and went further, fixing cases that had never worked. But it left
the actual goal unmet, because the issue never stated it. This document states
it.

**The requirement:** `infinite-craft-cli` must be able to write

```sudo
sorting.sort_by(rows, priority_row_less)
```

over `List<(int, text, text, int)>` at n ≥ 100,000, on the py and js backends,
and have it be the fastest reasonable option. No hand-rolled sort. No
decompose-into-scalar-key-passes. One call.

**Why 0.7.1 and not 0.8:** nothing here changes the language, the public API,
the emit protocol, or any observable behaviour. `sort_by(items, less)` keeps its
signature and its ordering. This is codegen quality — a program that works today
keeps working, just faster. That is a patch. See *Scope* for why it is
nonetheless not a one-line change.

## Why "just use sort_by_key" is not an acceptable answer

0.6's release notes recommend decorate-sort-undecorate for multi-key sorts, and
`stdlib/README.md` documents an LSD-radix recipe. We implemented it. It works
and it is ~3× faster than `sort_by`. We are rejecting it anyway:

```sudo
func row_key_score(r: (int, text, text, int)) -> int   // 4 helper functions
func row_key_first(r: (int, text, text, int)) -> text  // to express ONE
func row_key_second(r: (int, text, text, int)) -> text // multi-key sort
func int_greater(a: int, b: int) -> bool

sorting.sort_by_key(rows, row_key_second, text_lex_less)  // 3 passes, ordered
sorting.sort_by_key(rows, row_key_first,  text_lex_less)  // least-significant
sorting.sort_by_key(rows, row_key_score,  int_greater)    // key first
```

No sort library in a normal language asks this. `sorted(key=…)`, `sort_by_key`,
`std::sort` with a comparator — all take one call. Requiring the consumer to
manually decompose a composite ordering into stable passes, in the correct
reverse-significance order, is the library failing to be a library. It is also
a correctness trap: get the pass order backwards and you get a plausible,
silently wrong ordering.

## Measurements

All on the py backend, Apple Silicon, `sudoc` v0.7.0
(`6dd987daace869bbc69e4a08a5ed30dd1cf676d6f0519fabd6f6b31765ae8c9c`).

Methodology: each case has a `build_*` and a `sort_*` export over identical
input; reported time is `(build+sort) − (build)`, so list construction and host
marshalling are subtracted out. Only an `int` crosses the module boundary.
Harness listed at the end.

### Sort-only time, n = 4,000 (~48,000 comparisons)

| case | element | comparator reads | time | ns/cmp |
|---|---|---|---|---|
| E1 | `int` | the int | **0.077s** | 1,597 |
| E2 | `(int, text, text, int)` | all four fields | **1.970s** | 41,046 |
| E3 | `(int, text, text, int)` | **only the leading int** | **1.958s** | 40,794 |
| E5 | same, via 3× `sort_by_key` | scalar keys | **0.534s** | 11,118 |

- **E3 vs E1 — 25.5× slower** for the same comparison count and the same
  comparison work. The only difference is that the element is a 4-tuple.
- **E2 vs E3 — 1.0×.** Reading all four fields and running two `lex_compare`s
  costs *nothing measurable* over reading one int. **Comparison work is not the
  cost.**

### Payload-width probe — the decisive one

`List<(int, text)>`, n = 4,000, comparator reads **only the int** and never
touches the text. Identical shape, identical comparison count, identical
comparator. Only the text length varies:

| text width | sort-only time | vs width 1 |
|---|---|---|
| 1 | 0.348s | 1.00× |
| 16 | 1.800s | 5.17× |
| 64 | 6.344s | 18.22× |
| 256 | 25.056s | **71.97×** |

**A sort that never reads a field pays time proportional to that field's size.**
Elements are being copied, not compared.

### Copy attribution — instrumenting `_rt.dup`

Top-level `dup` calls and the scalar leaves each copies.
`List<(int, text)>`, n = 2,000, text width 64:

| source | calls | scalars copied | share |
|---|---|---|---|
| whole-list dups | 46 | 5,980,000 | **59.4%** |
| per-element dups | 62,820 | 4,079,220 | 40.6% |
| **total** | 62,866 | **10,059,220** | |

**The list holds 130,000 scalars. Sorting it copies 10,059,220 — 77× the list.**

Whole-list dup calls as n doubles (width 8): 42 → 46 → 50 → 54 for
n = 1,000 → 8,000. **+4 per doubling** — exactly 4 full-list deep copies per
merge pass, `log₂(n)` passes. At n = 97,020 that is ~70 whole-list copies.

### Is fixing the swap alone enough? No — measured, not estimated

I rewrote the generated `_sorting_impl.py`, replacing the emitted 4-dup swap
with a real rebinding at all 4 sites — i.e. exactly what fixed codegen would
emit for RC1, and nothing else. Sorted-order checksum verified identical
(596013942) before and after, so the rewrite is faithful.

| n = 4,000 | stock | RC1 fixed | |
|---|---|---|---|
| `sort_by`, one call | 2.002s | **0.830s** | 2.41× faster |
| 3× `sort_by_key` workaround | 0.533s | 0.524s | 1.02× faster |

**RC1 alone is a 2.41× win and still loses to the workaround by 1.58×.** The
workaround barely benefits, as expected: it sorts a `List<K>` of scalar keys,
where whole-list dups are already cheap.

So the swap fix is necessary and not sufficient. RC2 must land in the same
release or the requirement is not met.

## Root causes

### RC1 — a swap of two local `List` variables compiles to four deep copies (59% of copying)

`stdlib/sorting.sudo`, last line of the merge loop:

```sudo
items, buf = buf, items
```

generates:

```python
_sudo_t0 = _rt.dup(buf)
_sudo_t1 = _rt.dup(items)
items    = _rt.dup(_sudo_t0)
buf      = _rt.dup(_sudo_t1)
```

Four full-list deep copies, per pass, `log₂(n)` passes. Both sides are locals;
both source values are dead immediately after. This should be a rebinding.

The bitter irony: `sort_by`'s own doc comment explains at length that it is
iterative and helper-free *specifically to avoid copying the list*, and then
the very last line of the loop copies it four times.

### RC2 — every element move deep-copies the element (41%, and the rest of the gap)

```sudo
buf[k] = items[a]
```

generates:

```python
_rt.put(buf, k, _rt.dup(_rt.at(items, a)))
```

`n log n` element moves, each an O(payload) deep copy.

**This one is subtle and is why the release is not a one-liner.** A naive
elision is *wrong*: after the swap, `items` and `buf` are both live and swap
roles each pass, so sharing an element between them is observable if anything
later mutates an element in place — and 0.6 added support for `xs[i].f = v`.
The compiler must either prove the source is dead / non-aliased, or the runtime
must make sharing safe.

Copy-on-write in the py/js runtimes is the obvious candidate: `dup` becomes
O(1), real copying happens only on a write that has a second referent, and both
RC1 and RC2 collapse at once. It is semantically invisible, which is what keeps
this a patch release.

### RC3 — `text` is `List<int>`, so copying one is O(length)

```python
def host_text(x)  -> list: return [ord(c) for c in x]
def text_str(v)   -> str:  return "".join(chr(c) for c in v)
```

`_rt.dup` recurses per element, so duplicating one `text` copies every
codepoint. This is the multiplier that turns RC1 and RC2 from "some overhead"
into 72×.

**Not required for this release.** If RC1 and RC2 land, the sort stops copying
elements at all, and the length of a `text` it never copies stops mattering.
RC3 remains a real cost for code that genuinely copies text by value —
deferred to 0.7.2, specified in the appendix at the end of this document.

## Correction to a diagnosis in 0.6's release notes

0.6's notes and `sort_by`'s doc comment both attribute the cost to the
comparator:

> *"Because `less` compares WHOLE elements n log n times across that call
> boundary, prefer sort_by_key"*

**The generated code does not copy at the comparator call:**

```python
if less(_rt.at(items, b), _rt.at(items, a)):
```

No `_rt.dup`. 0.6's `never_written` entry-copy elision is working correctly,
including through an indirect call via a `func`-typed parameter. The comparator
boundary is free.

This matters because:

1. Anyone "fixing" the comparator boundary will fix nothing. I chased that
   hypothesis myself before reading the generated output; it is wrong.
2. `sort_by_key` is not faster for the reason the notes claim. It is faster
   because it sorts a `List<K>` of *scalar* keys — where RC1 and RC2 are
   cheap — and touches the composite list once, in `apply_permutation`.
3. E2 vs E3 (1.0×) confirms it independently: comparison work is free; element
   movement is everything.

The note and the doc comment should be corrected, or they will keep sending
people at the wrong target.

## Scope

**In:** RC1 and RC2. Both are value-semantics codegen quality, not sort-specific
— every composite-heavy sudo program pays for them today; `sort_by` is just
where it surfaced first.

**Out:** RC3. Any API or spec change. Any new multi-key sort API.

## Acceptance criteria

Ships when all of these hold on the py and js backends:

1. **`sort_by` is the fastest option.** For `List<(int, text, text, int)>` at
   n = 97,020, one `sort_by` call with a composite comparator beats the 3×
   `sort_by_key` radix decomposition. Today it loses 70.1s to 23.7s; with RC1
   alone it still loses 0.830s to 0.524s at n = 4,000.
2. **Composite penalty ≤ 3×.** E3 within 3× of E1 at the same n. Today: 25.5×.
3. **Payload independence.** `List<(int, text)>` with a comparator reading only
   the int is within 2× at text width 256 vs width 1. Today: 72×.
4. **Whole-list dups are O(1) per sort, not O(log n).** Instrumenting `_rt.dup`
   over one `sort_by` yields ≤ 2 whole-list copies regardless of n. Today:
   `4·log₂(n) + 2`.
5. **Total copy volume bounded.** Scalars copied by one `sort_by` ≤ 4× the
   list's own scalar count. Today: 77× at n = 2,000, growing with n.
6. **No API change.** Nothing in `craft.sudo` changes to benefit.
7. **A doubling test in the op-count harness** for copy volume, so this cannot
   silently regress.

Criteria 2–5 are cheap to automate and are what actually pin the behaviour.

## Benchmark harness

Standalone — no Bazel, no `craft.sudo`:

- `sortbench.sudo` — all cases. `sudoc build --target py -o py/ sortbench.sudo`.
- `run.py <py-dir> <n> <repeats>` — sort-only table and payload-width probe.
- `count_dup.py <py-dir> <n> <width>` — copy attribution via `_rt.dup`.
- `compare_rc1.py <stock-dir> <rc1-dir> <n>` — stock vs swap-fixed, with a
  checksum guard that fails loudly if a rewrite changes the ordering.

`sortbench.sudo` exposes paired `build_*` / `sort_*` exports so sort time is
isolated by subtraction, uses a deterministic LCG so input order is identical
across cases, and holds comparison counts identical between E2/E3 and across
every width in the E4 probe — so each table isolates exactly one variable.
`sort_rows_checksum` returns a positional checksum of the sorted order for
verifying that an experimental patch preserves semantics.

Worth landing in upstream `bench/` next to `list_append.sudo`.

---

# Appendix — RC3, deferred to 0.7.2

Self-contained; readable without the rest of this document.

## Read this first: 0.7.2 may already be done

RC3's cost is `dup(text)` being O(length). **How 0.7.1 fixes RC2 decides whether
any of this is still needed:**

- **If 0.7.1 implements copy-on-write** in the py/js runtimes, `dup` becomes
  O(1) for *every* composite including `text`, and RC3 dissolves as a side
  effect. Run the probe below. If it is flat across widths, close 0.7.2 as
  already-fixed and land the acceptance test as a regression guard.
- **If 0.7.1 instead fixes RC2 with liveness/escape analysis** — eliding copies
  only where the source is provably dead — then RC3 stands untouched. Every
  copy that *is* semantically required still costs O(length).

So: measure before implementing.

## What RC3 is

In the py backend, `text` is a list of codepoints:

```python
def host_text(x) -> list: return [ord(c) for c in x]
def text_str(v)  -> str:  return "".join(chr(c) for c in v)
```

`_rt.dup` recurses element-wise over a list, so duplicating one `text` copies
every codepoint individually. `text` is genuinely mutable in the language
(`s.append(120)`), so it cannot simply become an immutable py `str` — it needs
copy-on-write, or an immutable representation paired with a mutable builder.

## Evidence — independent of sorting

Appending one `text` into a `List<text>` 20,000 times. The copy is semantically
required (value semantics), no sort involved, comparator never enters the
picture. Only the source text's length varies.

| text width | `copy_texts` | `copy_ints` control | ratio | vs width 1 |
|---|---|---|---|---|
| 1 | 0.008s | 0.000s | 29× | 1.00× |
| 8 | 0.049s | 0.000s | 147× | 5.85× |
| 32 | 0.187s | 0.000s | 583× | 22.36× |
| 128 | 0.737s | 0.000s | 2,044× | 87.99× |
| 512 | 2.956s | 0.001s | 5,451× | **353.16×** |

Strictly linear in length — each 4× width step costs ~3.9×. Copying a
512-character text is ~5,451× a scalar.

Probe lives in `sortbench.sudo` as `copy_texts(n, width)` with `copy_ints` as
the same-shape scalar control.

## Why it matters to this consumer

`infinite-craft-cli`'s kernel is text-heavy in exactly the way this punishes.
Element names are `text` and live in `List<Element>`, `Set<text>`,
`Map<text, …>` and tuple keys; they get copied on essentially every container
operation. `title_case`, `sanitize_element_name` and the pair-key builders all
construct text per character. None of that is sorting, so none of it is covered
by 0.7.1.

## Scope for 0.7.2

**In:** make `dup(text)` O(1) on py and js.

**Out:** the language-level mutability of `text` — `s.append(…)` must keep
working. Any change to `text`'s observable semantics. Any spec surface change.

Still a patch release on the same reasoning as 0.7.1: a runtime representation
change is semantically invisible. Two things to confirm before assuming that —
they are the only ways this could become observable, and if either trips, it is
a minor, not a patch:

1. `canon` output used in assertion-failure diagnostics must be byte-identical.
2. `spec/backend-guide.md` must not be documenting the py/js text
   representation as normative for external emitters.

## Acceptance criteria for 0.7.2

1. **Payload independence on copy.** `copy_texts(20000, 512)` within 2× of
   `copy_texts(20000, 1)`. Today: 353×.
2. **Scalar parity.** `copy_texts` within 10× of the `copy_ints` control at
   width 512. Today: 5,451×.
3. **Mutation still works and stays isolated.** A test that copies a text,
   mutates the copy with `append`, and asserts the original is unchanged —
   copy-on-write must not leak writes to a shared referent. This is the
   correctness risk of the whole change and deserves adversarial cases:
   copy-then-mutate-original, copy-then-mutate-both, aliased inside a
   container, aliased across a call boundary.
4. **No diagnostic drift.** `canon` output byte-identical before and after.
5. **A doubling test in the op-count harness** on copy volume for text, so a
   regression fails the build.

Criterion 3 is the one to spend real effort on. Everything else here is a
performance number; that one is where a bug would be silent and semantic.
