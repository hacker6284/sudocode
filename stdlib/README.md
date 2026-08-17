# The sudo standard library

Libraries written in sudo itself — the project's own thesis, dogfooded:
one implementation, lockstep-verified across every backend, zero per-target
runtime additions.

| Module | What it provides |
|---|---|
| `sorting.sudo` | Generic stable `sort_by` (bottom-up merge sort, Theta(n log n)), key-based `sort_by_key` / `sort_index_of_keys` / `apply_permutation`, `lower_bound` / `binary_search`, `is_sorted_by`, `minimum_by`/`maximum_by`, `reversed` — monomorphized per element type |
| `strings.sudo` | The string library pseudocode hand-waves: `lex_compare`/`lex_less`, `split`/`split_str`/`join`, `index_of`/`last_index_of`/`contains`, `starts_with`/`ends_with`/`strip_prefix`/`strip_suffix`, `substring`/`replace`/`repeat`/`pad_left`/`pad_right`, `trim`/`trim_left`/`trim_right`/`is_space`, `to_upper`/`to_lower` (ASCII only) |
| `bigint.sudo` | Arbitrary-precision integers (sign + base-10⁹ limbs): add/sub/mul, `big_pow`, small-divisor divmod, decimal text round-trips. The escape hatch for algorithms that outgrow the trapping 64-bit `int` — `factorial(21)` traps; `factorial(21)` over BigInt just works |

Each module carries its own `test` blocks; `bazel test //stdlib/...` runs
them in lockstep across all seven backends.

## Multi-key sorting

Single-key decorate-sort-undecorate (`sort_by_key`) does not by itself
express "sort by score descending, then by text field A, then text field B,
then original index" — multiple keys with different priorities. The recipe
is an LSD (least-significant-digit-first) radix sort: call stable
`sort_by_key` once per key, applied from the **least** significant key to
the **most** significant key. Each later (higher-priority) pass's stability
preserves the relative order established by earlier passes as an automatic
tiebreaker.

This is legal **only** because `sort_by_key` is stable. On py/js it is
**not** the faster option for a composite row: `sort_by` with one
comparator is. (0.6's notes blamed the comparator call; the generated
code does not copy there. The real cost was moving elements — whole-list
dups in the merge ping-pong, plus a deep copy per `buf[k] = items[a]`.
0.7.1 made the swap a rebinding and introduced conditional COW;
0.7.3 makes every composite a COW handle — `dup` is an O(1) share for
lists, maps, sets, and records, including `List<text>` and
`List<record>` — and skips `dup` on a read-only destructure.)
Keep this recipe when you already think in keys, or when targeting a
backend that still deep-copies lists. It is not required to make
`sort_by` usable.

Comparators and key extractors are ordinary top-level functions and can be
passed **across modules** as values (e.g.
`sorting.sort_by_key(rows, my_key, sorting.int_ascending)` or
`sorting.sort_by(words, strings.lex_less)`). You do not need to redefine
`int_ascending` / `lex_less` (or your own key extractors) in every module
that wants an LSD multi-key recipe — import `std.sorting` / `std.strings`
and pass the qualified names straight through.

A caller-declared `record` or `enum` is a legal type argument:
`sorting.sort_by(xs, less)` with `xs: List<Thing>` compiles. The
instantiation still lives in `sorting`; `Thing` is placed in the shared
`sudo_types` unit so every backend can name it.
