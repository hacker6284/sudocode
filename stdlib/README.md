# The sudo standard library

Libraries written in sudo itself — the project's own thesis, dogfooded:
one implementation, lockstep-verified across every backend, zero per-target
runtime additions.

| Module | What it provides |
|---|---|
| `sorting.sudo` | Generic stable `sort_by` (bottom-up merge sort, Theta(n log n)), key-based `sort_by_key` / `sort_index_of_keys` / `apply_permutation`, `lower_bound` / `binary_search`, `is_sorted_by`, `minimum_by`/`maximum_by`, `reversed` — monomorphized per element type |
| `strings.sudo` | The string library pseudocode hand-waves: `lex_compare`, `split`/`join`, `index_of`/`contains`, `starts_with`/`ends_with`, `to_upper`/`to_lower` |
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

This is legal **only** because `sort_by_key` is stable. It is a large
constant-factor win (k single-field comparisons per element-pair instead of
one expensive whole-row `lex_compare`-style comparison), **not** an
asymptotic complexity win — both approaches are Theta(n log n) comparisons
total; radix-by-`sort_by_key` just makes each comparison k times cheaper
when the fields have very different comparison costs (e.g. int compare vs
text `lex_compare`).

Comparators and key extractors are ordinary top-level functions and can be
passed **across modules** as values (e.g.
`sorting.sort_by_key(rows, my_key, sorting.int_ascending)` or
`sorting.sort_by(words, strings.lex_less)`). You do not need to redefine
`int_ascending` / `lex_less` (or your own key extractors) in every module
that wants an LSD multi-key recipe — import `std.sorting` / `std.strings`
and pass the qualified names straight through.
