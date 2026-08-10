"""Complexity regression: bigint.big_to_text must be O(limb count), not
O(n^2). Uses runtime op counting -- see
stdlib/complexity/join_driver.py's docstring for the mechanism."""
import importlib
import sys


def measure(big_to_text, BigInt, rt, n):
    a = BigInt(False, [123456789] * n)  # negative=False, n valid base-1e9 limbs
    rt.reset_op_counts()
    out = big_to_text(a)
    assert len(out) > 0
    counts = rt.op_counts()
    return counts["add"] + counts["append"]


def main():
    gen_dir, small_n, large_n, bound = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
    sys.path.insert(0, gen_dir)
    impl = importlib.import_module("_bigint_impl")
    rt = importlib.import_module("_sudo_rt")

    small_ops = measure(impl.big_to_text, impl.BigInt, rt, small_n)
    large_ops = measure(impl.big_to_text, impl.BigInt, rt, large_n)
    ratio = large_ops / small_ops
    print(f"big_to_text ops: n={small_n} -> {small_ops}, n={large_n} -> {large_ops}, ratio={ratio:.3f} (bound {bound})")
    if ratio > bound:
        print(f"FAIL: op-count ratio {ratio:.3f} exceeds bound {bound} -- big_to_text looks worse than linear", file=sys.stderr)
        sys.exit(1)
    print("PASS")


if __name__ == "__main__":
    main()
