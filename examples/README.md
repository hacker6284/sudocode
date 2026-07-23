# Examples

Classic algorithms as living spec anchors — each parses, typechecks, and
passes its tests in every backend, and each doubles as documentation of what
idiomatic sudo looks like.

`gcd`, `binary_search`, `insertion_sort` (in-place via `inout`), `quicksort`
(Lomuto, recursion), `two_sum` (Map + tuples + Option), `bst` (recursive
tagged union + match), `bfs` (Map/Set/queue, deterministic despite unordered
containers — study it to see how), `palindrome` (text as scalar lists).

`quine.sudo` is a self-reproducing program: `quine()` returns this file's own
source, byte for byte. sudo is pure (no I/O), so it can't *print* itself — it
returns its source as `text` instead. Because one sudo program transpiles to
seven languages with identical semantics, the generated Python, C, JS, Swift,
Rust, Zig, and Haskell each return the same sudo source, and the in-file
`assert quine() == …` is `ok` across all seven — the lockstep certificate that
it really is a quine everywhere at once. (A classic self-referential-literal
form is provably impossible under sudo's purity — a quoted literal is always
longer than its value — so the self-reference closes through an `expand`
function over a two-marker template.) It doubles as a stringent text/escaping
stress test.

`pitfalls/order_dependent.sudo` is deliberately buggy: it lets unspecified
Map iteration order leak into a result, so `sudoc test` reports a divergence
between backends — the flagship failure mode the lockstep harness exists to
catch, kept here as a permanent demonstration. Its second test shows the fix
(sort first).
