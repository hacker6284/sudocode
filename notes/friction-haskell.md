# Friction log: Haskell external backend (`backends/haskell/`)

Notes from implementing the Haskell external backend against the protocol v1
JSON emit API, IR dumps, `spec/lockstep.md`, and the harness. Toolchain: **GHC**
(whatever the recipe invokes; harness recipe is `ghc -O0 -package containers`).
Definition of done: `sudoc conformance --external backends/haskell/hs.sudoc-backend.json`
is **9/9**; `sudoc test … examples/*.sudo stdlib/*.sudo` is fully green including
`stdlib/bigint.sudo`.

Layout:

| File | Role |
|---|---|
| `hs.sudoc-backend.json` | External-backend manifest (emit command, recipe, runtime copy) |
| `Emit.hs` | stdin JSON → generated `.hs` files (protocol response) |
| `SudoJson.hs` | Minimal JSON parse/render (no aeson dependency) |
| `SudoRt.hs` | Shared runtime: traps, lists/maps/sets, Option/Result, Canon, TAP |

---

## Spec / wire surprises

0. **TAP names are `test_*` function names, not `IrTest.name`.** Lockstep matches
   outcomes by `sudoc_ir::names::test_fn_names`. Printing
   `ok 1 - floor division and modulo` yields `no result (runner crashed?)` for
   every test even when the binary exits 0. Must print
   `ok 1 - test_floor_division_and_modulo`.

1. **`let !n = n + 1` is a black hole.** Haskell `let` is recursive. Bang patterns
   do not make bindings non-recursive. Every statement-level rebinding must use
   `case e of { !n -> ... }` (RHS sees the outer `n`). This was the single
   largest codegen bug; it produced `<<loop>>` on every loop test until fixed.
   The original task prompt’s “always bang-pattern let” guidance is correct
   *intent* (force WHNF) but must not be taken as literal `let` syntax in Haskell.

2. **`data Box { field :: T }` is invalid.** Record syntax needs a constructor:
   `data Box = Box { box_items :: T }`. Easy to miss if you only look at field
   accessors.

3. **Function-typed parameters need parentheses in signatures.** Rendering
   `TFunc` as `Int64 -> Int64 -> Bool` without parens flattens into multiple
   curried params of the *host* function. Must emit
   `(Int64 -> Int64 -> Bool)`.

4. **Prelude name clashes.** Sudo does not reserve `take`, `map`, `length`,
   etc. Unqualified import of the entry module into `*_test.hs` collides with
   Prelude. Defensive mangling of common Prelude names (append `_`) is required
   beyond the Haskell 2010 reserved-word list.

5. **MutBuiltin can nest inside expressions.** e.g. `assert a.pop() == 1` has
   `ListPop` as the LHS of `==` inside `Assert.cond`. Statement-only handling is
   not enough; need a hoist pass that materializes MutBuiltin into binds before
   the pure expression.

6. **Module name capitalization.** Sudo modules are lowercase (`arithmetic`);
   Haskell modules must start uppercase → `T_Arithmetic` via the same
   type-name mangling. File name must match (`T_Arithmetic.hs`). Recipe only
   compiles `{entry}_test.hs`; GHC finds the module by name in the cwd. Works.

7. **`MapGet` returns Option.** Surface `.get` is `MapGet` → `Rt.mapGetOpt`,
   not the trapping `mapGet` used for `m[k]` index syntax.

8. **Inout + MutBuiltin must enter loop threaded-vars.** A loop that only
   mutates via `items.swap(...)` (MutBuiltin Expr stmt) never has
   `declares: false` Assign on `items`; must collect MutBuiltin receiver roots
   (and inout writeback targets) when computing `go` parameters. Without this,
   `sort_by` silently returns the original list.

9. **User records/enums need generated `Rt.Canon` instances.** Session 1’s 9
   conformance modules rarely (if ever) `assert x == y` on a whole user record;
   `stdlib/bigint.sudo` does (`assert big_sub(big_add(a,b), b) == a`). Failure:
   `No instance for 'Rt.Canon BigInt' arising from a use of 'Rt.sudoAssertEq'`.
   Root cause: `emitRecord`/`emitEnum` only emitted `deriving (Eq, Ord, Show)` —
   never `instance Rt.Canon …`. Fix is general: every user record/enum gets a
   Canon instance at declaration site, using lockstep diagnostic shape
   (`{"r": "Name", "v": […]}` / `{"e": "Enum.Variant", "v": […]}`) via
   `Rt.canonRecord` / `Rt.canonEnum` helpers so generated modules need no
   `Data.List` import. Nested fields/variants just call `Rt.canon` on components;
   GHC instance resolution handles recursive types and
   `[T]` / `SOption T` wrappers.

---

## Runtime notes

- Haskell `div`/`mod` on `Int64` already implement floor division/modulo
  (verified via conformance arithmetic tests).
- `Double` Eq is IEEE (`NaN /= NaN`, `-0.0 == 0.0`).
- Value semantics are free with immutable structures; no `dup`.
- `Data.Map` / `Data.Set` give order-insensitive Eq for free.
- Canon for primitives/containers lives in `SudoRt`; user types are codegen’d
  (see §9 above). Canon is diagnostic-only for lockstep v1.

---

## Codegen shape (readability)

Generated bodies are pure expression chains of forced binds, not do-notation
(except `expect_trap` tests, which need `IO`). Loops lower to a local recursive
helper plus `Rt.Cont`/`Rt.Brk`/`Rt.Ret`. Nested loops mint distinct helper names
(`go`, `go2`, `go3`, …) via `ctxFresh`; sibling loops may reuse `go` (separate
scopes). Inout calls return `(ret?, …inouts)` and write back with the same
case-bind style.

`examples/quicksort.sudo` → `partition` / `quicksort` / `quicksort_range` keep
those names (value mangling only hits reserved/Prelude/clash cases).

### Session 3 pretty-printer

Layout is home-grown (boot libraries only; no pretty-printer package):

| Helper | Role |
|---|---|
| `indN = 2` | fixed spaces per nest level |
| `indentAll n` | pad every line of a multi-line fragment |
| `bangBind` | layout-rule `case e of` / `!pat ->` (still non-recursive; see §1) |
| `caseOf` | multi-arm layout case; multi-line scrutinee wrapped in `(…)` |
| `layoutIf` | `if` / `then` / `else` each on their own indent band |
| `prettyLet` | `let lhs =` / indented rhs / `in` / indented body |
| `emitArg` / `emitOperand` / `parenIfApp` | stop paren-wrapping every atom |

Forced binds stay as **layout `case`**, not `let !x = e`: session 1’s black-hole
finding still applies when `e` mentions the name being bound. Fresh temps
(`_expectBody`, `_fromV`, …) could use `let`, but case is uniform and safe.

**Do-layout traps (expect_trap IO):**

1. Multi-line `evaluate (` … `)) :: IO …` as a `_r <-` RHS continuation at the
   *same* indent as the do-statement is parsed as a new statement →
   `parse error on input ')'`.
2. `let _expectBody =` followed by `case` at the *same column as the binding
   name* is parsed as a sibling declaration →
   `parse error (possibly incorrect indentation)`.

Fix: lazy `let _expectBody =` with RHS indented past the name column (no bang
— bang would force *outside* `try`), then a single-line
`try (evaluate _expectBody) :: IO …`.

---

## CLI: `--target` vs `--external`

```
sudoc build --external backends/haskell/hs.sudoc-backend.json --target hs -o … file.sudo
```

fails with `unknown target 'hs' (available: py, c, js, swift, rs, zig)` *before*
the external manifest is consulted. `--target` only knows backends compiled into
`sudoc`. External backends are selected solely via `--external`:

```
sudoc build --external backends/haskell/hs.sudoc-backend.json -o … file.sudo
```

Useful signal for the CLI: either document that external ids are not
`--target` values, or merge external target names into the same table.

---

## Operational (agent / grok CLI)

- Terminal harness cancels commands that combine pipes, `&&`/`;`, redirections,
  and heavy inline quoting. Workaround: write a small `/tmp/*.py` driver, then
  run `python3 /tmp/driver.py` as a **single plain command**.
- IR dumps under agent scratchpads were useful in session 1; session 2 mostly
  used live `sudoc test` / `conformance` against the real harness.

---

## Session status

### Session 1

- Architecture complete: protocol JSON, strict IR decode, runtime, expr/loop
  modes, Flow, inout writeback, MutBuiltin hoist, records/enums/maps/sets,
  Option/Result, expect_trap, TAP runner.
- Conformance 9/9 against the real harness.

### Session 2

- Fixed missing `Rt.Canon` instances for all user records/enums (bigint + any
  future `assert rec == rec`).
- Removed dead inout path leftovers (`emitInoutCall`/`writebackOne`/
  `rebuildFromExpr` pre-fix drafts collapsed to the single working path;
  dropped unused `restIfEmpty` / dead `armExprs` binding; removed session-1
  monkey-patch comments).
- Nested loop helpers renamed `go` / `go2` / `go3` / … (small change; kept).
- Deleted scratch `backends/haskell/_smoke_extract.py` (was an undocumented
  underscore-prefixed leftover next to real deliverables).
- Verified: conformance **9/9**; full `examples/*.sudo` + `stdlib/*.sudo` green.

### Session 3

- Pure readability pass: layout-rule `case`/`if`/`let`, 2-space indent, arg
  paren hygiene (`emitArg` / `emitOperand`). No semantics/runtime changes
  intended.
- Hit do-layout parse error on multi-line `expect_trap` evaluate wrappers;
  fixed via lazy let-bound body before single-line `try`/`evaluate`.
- Verified: conformance **9/9**; full `examples/*.sudo` + `stdlib/*.sudo` green.

### Session 4

Pure readability peephole pass on emit output (no runtime/IR/manifest changes).
Introduced a small body IR (`Hs` / `FPat`) over forced-bind chains, ran
simplifications to a fixed point, then pretty-printed with the session-3 layout
helpers.

| Peephole | What it does | Where it shows up |
|---|---|---|
| 1. Collapse double-hops | `case E of !tmp -> case tmp of !x -> B` → `case E of !x -> B`; tuple writeback `!(_ret,_io0)` + rebinds → `!(!p, !items)` (field bangs force each component at the same point as the old nested cases — verified legal with BangPatterns) | inout writeback, MutBuiltin `swapL`/`appendL`, chains of arbitrary length |
| 2. Never case a bare variable | `case v of !x -> B` → use `v` in `B` when `v` is already a plain var **and** `v` is not free in `B` (pure rename). Live copies (`b = a` then mutate `b`, still read `a`) are kept. | for-range `_fromV` when `from` is a param (`go lo …`); skipped on real copies |
| 3. Tail identity | `case E of !x -> x` → `E` | `quicksort` body is just the `quicksort_range` call; last recursive `quicksort_range` has no bind wrapper |
| 4. Drop redundant `:: Int64` | Bare numerals in monomorphic positions (`Rt.chk*`, user-fn args, list indices). Keep ann / list ascription where GHC needs a pin: `sudoAssertEq`, `EText`/`EList` of ints, polymorphic mut values (`setAdd`), map keys | `Rt.chkSub … 1`, `go (j + 1)`; still `(3 :: Int64)` in asserts |

**Order / safety notes (applied, not skipped silently):**

1. Peep 1 must run *before* peep 2 on a force-bind: otherwise peep 2 treats hop
   temps (`_io0`, `_ret`) as bare vars and substitutes them, destroying the hop
   shape peep 1 needs to rename the outer pattern to real names.
2. Peep 1 and peep 2 both require the intermediate name to be **dead** in the
   residual body (`freeInHs`). Without that, `case a of !b` after `a = …` is a
   live copy; collapsing it unbound `a` in asserts (structures / value_semantics
   regressions). Logged as a required free-var gate, not an unhandled shape.
3. `substHs` must not rename binders on `HLet` lhs or multi-arm case patterns
   when the name is shadowed (early bug: `go2 items i` → `go2 items j` while
   body still said `i`).
4. `HCase` scrutinees go through `caseOf` only — do not also wrap multi-line
   scrutinees in `renderForceScrut`, or you get double parentheses around loop
   bodies.

**Peepholes not skipped as unsafe** beyond the free-var gate above: field-bang
tuple patterns (`!(!p, !items)`) confirmed to force both fields; tail identity
preserves WHNF demand of the overall expression.

Verified: conformance **9/9**; full `examples/*.sudo` + `stdlib/*.sudo` green
(11 modules / 33 tests).

---

## Soft spots (not blockers)

- StackOverflow catchability not deeply tested.
- No export/boundary adapters (out of scope for external backend v1).
- Emitter still uses `head` on well-formed Option/Result construction args
  (host-side only; IR is trusted).
- Consider `import Prelude hiding (...)` as an alternative to value-name
  mangling for Prelude clashes — not required; mangling works.
- Multi-line scrutinees of Flow matches are parenthesized (`case ( if … ) of`);
  readable enough, could use a named `let` temp for even cleaner loop bodies.
- Multi-module type references currently render as bare mangled names (works
  when each type is defined in a co-compiled module imported with
  `import qualified`; instances still resolve). Cross-module *unqualified*
  type mentions in signatures could need qualification if the corpus grows
  multi-module user types in one compile unit.
