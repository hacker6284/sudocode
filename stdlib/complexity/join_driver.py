"""Complexity regression: strings.join must be O(total output length), not
O(n^2) -- commit 73c9b0a replaced an `out = out + p` accumulator with
out.append(c) per character. Uses runtime op counting (_sudo_rt.count_add /
count_append) -- this target's codegen step builds with SUDO_COUNT_OPS=1
(tools/complexity.bzl), which routes generated list `+` / `.append()` through
those counters. Off (and provably inert) in every other build in this repo."""
import importlib
import sys


def measure(join, rt, n):
    parts = [[ord(c) for c in "aaaaaaaaaaaaaaaaaaaa"] for _ in range(n)]  # 20 chars each
    sep = ord("|")
    rt.reset_op_counts()
    out = join(parts, sep)
    assert len(out) == n * 20 + max(n - 1, 0)
    counts = rt.op_counts()
    return counts["add"] + counts["append"]


def main():
    gen_dir, small_n, large_n, bound = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
    sys.path.insert(0, gen_dir)
    impl = importlib.import_module("_strings_impl")
    rt = importlib.import_module("_sudo_rt")

    small_ops = measure(impl.join, rt, small_n)
    large_ops = measure(impl.join, rt, large_n)
    ratio = large_ops / small_ops
    print(f"join ops: n={small_n} -> {small_ops}, n={large_n} -> {large_ops}, ratio={ratio:.3f} (bound {bound})")
    if ratio > bound:
        print(f"FAIL: op-count ratio {ratio:.3f} exceeds bound {bound} -- join looks worse than linear", file=sys.stderr)
        sys.exit(1)
    print("PASS")


if __name__ == "__main__":
    main()
