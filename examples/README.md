# Examples

Classic algorithms as living spec anchors — each parses, typechecks, and
passes its tests in every backend, and each doubles as documentation of what
idiomatic sudo looks like.

`gcd`, `binary_search`, `insertion_sort` (in-place via `inout`), `quicksort`
(Lomuto, recursion), `two_sum` (Map + tuples + Option), `bst` (recursive
tagged union + match), `bfs` (Map/Set/queue, deterministic despite unordered
containers — study it to see how), `palindrome` (text as scalar lists).

`pitfalls/order_dependent.sudo` is deliberately buggy: it lets unspecified
Map iteration order leak into a result, so `sudoc test` reports a divergence
between backends — the flagship failure mode the lockstep harness exists to
catch, kept here as a permanent demonstration. Its second test shows the fix
(sort first).
