# The sudo standard library

Libraries written in sudo itself — the project's own thesis, dogfooded:
one implementation, lockstep-verified across every backend, zero per-target
runtime additions.

| Module | What it provides |
|---|---|
| `sorting.sudo` | Generic stable `sort_by`, `is_sorted_by`, `minimum_by`/`maximum_by`, `reversed` — monomorphized per element type |
| `strings.sudo` | The string library pseudocode hand-waves: `lex_compare`, `split`/`join`, `index_of`/`contains`, `starts_with`/`ends_with`, `to_upper`/`to_lower` |
| `bigint.sudo` | Arbitrary-precision integers (sign + base-10⁹ limbs): add/sub/mul, `big_pow`, small-divisor divmod, decimal text round-trips. The escape hatch for algorithms that outgrow the trapping 64-bit `int` — `factorial(21)` traps; `factorial(21)` over BigInt just works |

Each module carries its own `test` blocks; `sudoc test stdlib/*.sudo` runs
them in lockstep across all installed targets.
