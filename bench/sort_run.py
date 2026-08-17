#!/usr/bin/env python3
"""Run the sort isolation benchmark and print a table.

Each row reports SORT-ONLY time: (build+sort) - (build), best of R repeats,
so list construction and any host marshalling are subtracted out.

Usage (from repo root, after `sudoc build --target py -o /tmp/sortbench bench/sortbench.sudo`):

    python3 bench/sort_run.py /tmp/sortbench 4000 3
"""
import math
import sys
import time

sys.path.insert(0, sys.argv[1])
import sortbench as sb  # noqa: E402

REPEATS = int(sys.argv[3]) if len(sys.argv) > 3 else 3


def best(fn, *a):
    t = math.inf
    for _ in range(REPEATS):
        t0 = time.perf_counter()
        fn(*a)
        t = min(t, time.perf_counter() - t0)
    return t


def sort_only(sort_fn, build_fn, *a):
    return max(0.0, best(sort_fn, *a) - best(build_fn, *a))


def ncmp(n):
    """Merge sort comparison count, ~n*ceil(log2 n) upper bound."""
    return n * math.ceil(math.log2(n)) if n > 1 else 0


N = int(sys.argv[2])
print(f"n = {N}   (~{ncmp(N):,} comparisons for an n log n sort)   best of {REPEATS}\n")

rows = []

t_int = sort_only(sb.sort_int, sb.build_int, N)
rows.append(("E1  sort_by  List<int>                scalar element", t_int))

t_full = sort_only(sb.sort_rows_full, sb.build_rows, N)
rows.append(("E2  sort_by  List<(int,text,text,int)> comparator reads all", t_full))

t_io = sort_only(sb.sort_rows_int_only, sb.build_rows, N)
rows.append(("E3  sort_by  List<(int,text,text,int)> comparator reads 1 int", t_io))

t_key = sort_only(sb.sort_rows_bykey, sb.build_rows, N)
rows.append(("E5  3x sort_by_key on scalar keys      (same final order)", t_key))

w = max(len(r[0]) for r in rows)
for label, t in rows:
    per = (t / ncmp(N) * 1e9) if ncmp(N) else 0
    print(f"  {label:<{w}}  {t:8.3f}s   {per:7.0f} ns/cmp")

if t_int:
    print(f"\n  E3 vs E1  composite element, same comparison count: {t_io / t_int:.1f}x slower")
if t_io:
    print(f"  E2 vs E3  cost of actually reading the text fields: {t_full / t_io:.1f}x")
if t_key:
    print(f"  E2 vs E5  sort_by vs 3x sort_by_key: {t_full / t_key:.1f}x")

cs_by = sb.sort_rows_checksum(N)
cs_key = sb.sort_rows_bykey_checksum(N)
print(f"\n  checksum sort_by={cs_by}  3x sort_by_key={cs_key}")
if cs_by != cs_key:
    print("FAIL: sort_by and 3x sort_by_key produced different orderings", file=sys.stderr)
    sys.exit(1)

print("\n  E4  payload-width probe — List<(int,text)>, comparator reads only the int")
base = None
for width in (1, 16, 64, 256):
    t = sort_only(sb.sort_pairs, sb.build_pairs, N, width)
    if base is None:
        base = t
    ratio = t / base if base else 0
    print(f"      text width {width:>4}  {t:8.3f}s   {ratio:5.2f}x vs width 1")

print("\n  copy_texts / copy_ints — required dup(text), no sort")
ct_base = None
t_ct_512 = None
t_ci_512 = None
for width in (1, 128, 512):
    t_ct = best(sb.copy_texts, 20000, width)
    t_ci = best(sb.copy_ints, 20000, width)
    if ct_base is None:
        ct_base = t_ct
    if width == 512:
        t_ct_512 = t_ct
        t_ci_512 = t_ci
    ratio = t_ct / ct_base if ct_base else 0
    print(
        f"      width {width:>4}  copy_texts {t_ct:8.3f}s   "
        f"copy_ints {t_ci:8.3f}s   {ratio:5.2f}x vs width 1"
    )
if t_ct_512 and t_ci_512:
    print(f"      copy_texts vs copy_ints at width 512: {t_ct_512 / t_ci_512:.1f}x")

print("\n  E7  same unread payload in List<text> and List<Row>")
for label, sort_fn, build_fn in (
    ("List<text>", sb.sort_texts, sb.build_texts),
    ("List<Row> ", sb.sort_records, sb.build_recs),
):
    base = None
    parts = []
    for width in (8, 64, 256):
        t = sort_only(sort_fn, build_fn, N, width)
        if base is None:
            base = t
        ratio = t / base if base else 0
        parts.append(f"w={width} {t:6.3f}s ({ratio:4.2f}x)")
    print(f"      {label}  " + "  ".join(parts))
