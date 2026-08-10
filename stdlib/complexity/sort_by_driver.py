"""Complexity regression: sorting.sort_by must be Theta(n log n) comparator
calls, not Theta(n^2). Counts comparator calls directly (sort_by takes
`less` as a plain callable) -- no codegen instrumentation needed; this works
against ANY sort_by implementation, old or new, unmodified."""
import importlib
import sys


def measure(sort_by, n):
    xs = list(range(n - 1, -1, -1))  # reverse-sorted: the old insertion
    # sort's worst case, and still a real workload for merge sort.
    count = [0]

    def counting_less(a, b):
        count[0] += 1
        return a < b

    # Python backend threads inout via the return value (items = sort_by(...)),
    # so the caller's list is not mutated in place — capture the result.
    xs = sort_by(xs, counting_less)
    assert xs == list(range(n)), "sort_by produced a wrong result"
    return count[0]


def main():
    gen_dir, small_n, large_n, bound = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
    sys.path.insert(0, gen_dir)
    impl = importlib.import_module("_sorting_impl")
    sort_by = impl.sudo_7sort_by__3i64

    small_ops = measure(sort_by, small_n)
    large_ops = measure(sort_by, large_n)
    ratio = large_ops / small_ops
    print(f"sort_by comparisons: n={small_n} -> {small_ops}, n={large_n} -> {large_ops}, ratio={ratio:.3f} (bound {bound})")
    if ratio > bound:
        print(f"FAIL: comparator-call ratio {ratio:.3f} exceeds bound {bound} -- sort_by looks worse than O(n log n)", file=sys.stderr)
        sys.exit(1)
    print("PASS")


if __name__ == "__main__":
    main()
