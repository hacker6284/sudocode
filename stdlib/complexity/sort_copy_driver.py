"""sort_by copy-volume growth.

Soul — this is the test. Keep it, even if every number below changes:

  When n doubles, whole-list copies stay flat (not +4 per doubling — that
  was the merge ping-pong). Leaf copies grow like n log n, not n² and not
  n log n × payload. Work per `dup` does not follow unread payload width:
  quadrupling a text field (or sorting `List<text>` / `List<record>` of
  wider texts) must not grow leaves OR list-dup count. `copy_texts` — a
  semantically-required dup of `text`, no sort — must also be independent
  of width. Unpack count (tuple dups), not unpack *cost*, scales with
  comparisons — reading a field does not scale with unread fields.

Everything else is a snapshot of the py runtime (uniform COW: dup is
always a share; read-only unpack without dup). A later impl that moves,
shares differently, or copies a different constant amount should edit
WIDTH / the slack / the printed numbers. Do not treat those as the
contract.

Wall-clock is rejected here as flaky. Counts come from `_sudo_rt.dup_stats`
after a sort-only / copy-only call (reset so build is not in the
measurement).
"""
import importlib
import sys

# Snapshot: wide enough that a payload-copying regression is visible
# when width quadruples; not part of the soul.
WIDTH = 8


def measure_sort(sort_pairs, rt, n, width):
    filler = rt.lst([ord("x")] * width)
    items = rt.lst([(i, filler) for i in range(n, 0, -1)])
    rt.reset_dup_stats()
    items = sort_pairs(items)
    got = [row[0] for row in items]
    assert got == list(range(1, n + 1)), "sort_pairs produced a wrong result"
    stats = rt.dup_stats()
    return (
        stats["list_by_len"].get(n, 0),
        stats["leaves"],
        stats["list"],
        stats.get("tuple", 0),
    )


def measure_copy_texts(copy_texts, rt, n, width):
    rt.reset_dup_stats()
    got = copy_texts(n, width)
    assert got == n, "copy_texts produced a wrong result"
    stats = rt.dup_stats()
    return stats["list"], stats["leaves"]


def measure_sort_texts(sort_texts, rt, n, width):
    filler = rt.lst([ord("x")] * width)
    items = rt.lst([filler for _ in range(n)])
    rt.reset_dup_stats()
    items = sort_texts(items)
    assert len(items) == n
    stats = rt.dup_stats()
    return stats["list"], stats["leaves"]


def measure_sort_wides(sort_wides, Wide, rt, n, width):
    filler = rt.lst([ord("x")] * width)
    items = rt.lst([rt.rec(Wide(i, filler)) for i in range(n, 0, -1)])
    rt.reset_dup_stats()
    items = sort_wides(items)
    got = [row.k for row in items]
    assert got == list(range(1, n + 1)), "sort_wides produced a wrong result"
    stats = rt.dup_stats()
    return stats["list"], stats["leaves"]


def fail_width(label, a, b, what):
    if a and b > a * 2:
        print(
            f"FAIL: {label} {what} grew {a} -> {b} when width quadrupled "
            f"(unread payload is still being walked)",
            file=sys.stderr,
        )
        sys.exit(1)


def main():
    gen_dir, small_n, large_n, bound = (
        sys.argv[1],
        int(sys.argv[2]),
        int(sys.argv[3]),
        float(sys.argv[4]),
    )
    sys.path.insert(0, gen_dir)
    impl = importlib.import_module("_sort_copy_volume_impl")
    rt = importlib.import_module("_sudo_rt")
    # Wide is a type argument to sorting.sort_by, so it lives in sudo_types.
    types = importlib.import_module("_sudo_types_impl")

    small_whole, small_leaves, _, small_tup = measure_sort(
        impl.sort_pairs, rt, small_n, WIDTH
    )
    large_whole, large_leaves, _, large_tup = measure_sort(
        impl.sort_pairs, rt, large_n, WIDTH
    )
    _, wide_leaves, _, wide_tup = measure_sort(
        impl.sort_pairs, rt, small_n, WIDTH * 4
    )

    print(
        f"sort_by copy growth: "
        f"n={small_n} whole={small_whole} leaves={small_leaves} tuple={small_tup}, "
        f"n={large_n} whole={large_whole} leaves={large_leaves} tuple={large_tup}, "
        f"width {WIDTH}->{WIDTH * 4} leaves={wide_leaves} tuple={wide_tup}"
    )

    # RC1: whole-list copies are O(1) per sort. Doubling n must not add
    # another pass of them. Today's snapshot is 1 and 1; 0 and 0 is better;
    # 3 and 3 is a different constant — change nothing here, the count did
    # not grow. 1 and 5 is the old 4·log n shape.
    if large_whole > small_whole:
        print(
            f"FAIL: whole-list dups grew {small_whole} -> {large_whole} "
            f"when n doubled (copy work is growing with pass count)",
            file=sys.stderr,
        )
        sys.exit(1)

    # Unread payload is not copied. Quadrupling text width must not scale
    # leaves. Slack 2× is for a later impl that still touches a constant
    # extra per element; 4× would mean the text is in the walk.
    fail_width("sort_pairs", small_leaves, wide_leaves, "leaves")

    # n log n leaf growth, not quadratic. Same bound the harness passes
    # for comparator-call doubling (~2.1–2.3 observed; 2.6 is the ceiling).
    ratio = large_leaves / small_leaves if small_leaves else 0.0
    print(f"  leaf ratio n->2n = {ratio:.3f} (bound {bound})")
    if small_leaves and ratio > bound:
        print(
            f"FAIL: leaf-copy ratio {ratio:.3f} exceeds bound {bound}",
            file=sys.stderr,
        )
        sys.exit(1)

    # Fix B: unpack *count* scales with comparisons, not with unread field
    # size. 0 and 0 is the current snapshot (read-only unpack skips dup).
    # A later impl that still dups the tuple may have a constant per cmp;
    # that count must not grow when width quadruples, and n->2n must stay
    # n log n.
    fail_width("sort_pairs", small_tup, wide_tup, "tuple dups")
    tup_ratio = large_tup / small_tup if small_tup else 0.0
    print(f"  tuple-dup ratio n->2n = {tup_ratio:.3f} (bound {bound})")
    if small_tup and tup_ratio > bound:
        print(
            f"FAIL: tuple-dup ratio {tup_ratio:.3f} exceeds bound {bound} "
            f"(unpack count is growing worse than comparisons)",
            file=sys.stderr,
        )
        sys.exit(1)

    # Required dup(text), no sort. Same n, width must not add dups OR leaves
    # (a deep walk of the text increments leaves by width).
    ct, ct_l = measure_copy_texts(impl.copy_texts, rt, small_n, WIDTH)
    ct_w, ct_lw = measure_copy_texts(impl.copy_texts, rt, small_n, WIDTH * 4)
    print(
        f"copy_texts: n={small_n} width={WIDTH} list={ct} leaves={ct_l}, "
        f"width={WIDTH * 4} list={ct_w} leaves={ct_lw}"
    )
    fail_width("copy_texts", ct, ct_w, "list dups")
    fail_width("copy_texts", ct_l, ct_lw, "leaves")

    # 3b / 3c: List<text> and List<record> must not pay unread width.
    st, st_l = measure_sort_texts(impl.sort_texts, rt, small_n, WIDTH)
    st_w, st_lw = measure_sort_texts(impl.sort_texts, rt, small_n, WIDTH * 4)
    print(
        f"sort_texts: width={WIDTH} list={st} leaves={st_l}, "
        f"width={WIDTH * 4} list={st_w} leaves={st_lw}"
    )
    fail_width("sort_texts", st, st_w, "list dups")
    fail_width("sort_texts", st_l, st_lw, "leaves")

    sw, sw_l = measure_sort_wides(impl.sort_wides, types.Wide, rt, small_n, WIDTH)
    sw_w, sw_lw = measure_sort_wides(
        impl.sort_wides, types.Wide, rt, small_n, WIDTH * 4
    )
    print(
        f"sort_wides: width={WIDTH} list={sw} leaves={sw_l}, "
        f"width={WIDTH * 4} list={sw_w} leaves={sw_lw}"
    )
    fail_width("sort_wides", sw, sw_w, "list dups")
    fail_width("sort_wides", sw_l, sw_lw, "leaves")
    print("PASS")


if __name__ == "__main__":
    main()
