"""sort_by copy-volume growth.

Soul — this is the test. Keep it, even if every number below changes:

  When n doubles, whole-list copies stay flat (not +4 per doubling — that
  was the merge ping-pong). Leaf copies grow like n log n, not n² and not
  n log n × payload. When the unread text field gets longer, the leaf
  count does not follow it.

Everything else is a snapshot of the 0.7.1 py runtime (COW lists, one
share for `buf = items`, tuple slots not walked). A later impl that
moves, shares differently, or copies a different constant amount should
edit WIDTH / the slack / the printed numbers. Do not treat those as
the contract.

Wall-clock is rejected here as flaky. Counts come from `_sudo_rt.dup_stats`
after a sort-only call (reset so build is not in the measurement).
"""
import importlib
import sys

# Snapshot: wide enough that a payload-copying regression is visible
# when width quadruples; not part of the soul.
WIDTH = 8


def measure(sort_pairs, rt, n, width):
    filler = rt.lst([ord("x")] * width)
    items = rt.lst([(i, filler) for i in range(n, 0, -1)])
    rt.reset_dup_stats()
    items = sort_pairs(items)
    got = [row[0] for row in items]
    assert got == list(range(1, n + 1)), "sort_pairs produced a wrong result"
    stats = rt.dup_stats()
    return stats["list_by_len"].get(n, 0), stats["leaves"]


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

    small_whole, small_leaves = measure(impl.sort_pairs, rt, small_n, WIDTH)
    large_whole, large_leaves = measure(impl.sort_pairs, rt, large_n, WIDTH)
    _, wide_leaves = measure(impl.sort_pairs, rt, small_n, WIDTH * 4)

    print(
        f"sort_by copy growth: "
        f"n={small_n} whole={small_whole} leaves={small_leaves}, "
        f"n={large_n} whole={large_whole} leaves={large_leaves}, "
        f"width {WIDTH}->{WIDTH * 4} leaves={wide_leaves}"
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

    # RC2: unread payload is not copied. Quadrupling text width must not
    # scale leaves. Slack 2× is for a later impl that still touches a
    # constant extra per element; 4× would mean the text is in the walk.
    if small_leaves and wide_leaves > small_leaves * 2:
        print(
            f"FAIL: leaves grew {small_leaves} -> {wide_leaves} when text "
            f"width quadrupled (elements are being copied, not moved)",
            file=sys.stderr,
        )
        sys.exit(1)

    # n log n leaf growth, not quadratic. Same bound the harness passes
    # for comparator-call doubling (~2.1–2.3 observed; 2.6 is the ceiling).
    ratio = large_leaves / small_leaves if small_leaves else float("inf")
    print(f"  leaf ratio n->2n = {ratio:.3f} (bound {bound})")
    if ratio > bound:
        print(
            f"FAIL: leaf-copy ratio {ratio:.3f} exceeds bound {bound}",
            file=sys.stderr,
        )
        sys.exit(1)
    print("PASS")


if __name__ == "__main__":
    main()
