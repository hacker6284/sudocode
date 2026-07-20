# The sudo Language Specification

Version 0.1. Everything in this document is implemented and enforced by the
conformance suite (`conformance/semantics/`), which is the executable form of
this spec — six backends must agree on all of it.

sudo is a statically typed, fully pure, value-semantics language whose surface
syntax stays as close as practical to CLRS-style textbook pseudocode. Every
construct has exactly one defined behavior; there is no implementation-defined or
undefined behavior except where explicitly called out (Map/Set iteration order).

---

## 1. Source form

- Encoding: UTF-8. Non-ASCII is permitted only inside comments and text literals.
- A source file is a **module**; the module name is the file name minus `.sudo`
  (`sorting.sudo` → `sorting`). Module names are identifiers.
- Comments run from `//` to end of line. There are no block comments.
- Statements are separated by newlines. There are no semicolons.
- **Blocks are defined by indentation** (CLRS style — no braces, no `end`, no
  colons). The indentation unit is 4 spaces; tab characters in leading whitespace
  are a compile error. A block header (`if`, `while`, `for`, `func`, `match`,
  `case`, `else`, `record`, `enum`, `test`) is followed by a newline and an
  indented block.
- A line may be continued after an unclosed `(`, `[`, or `{`, as in Python.
- Blank lines are insignificant.

### Identifiers

`[a-zA-Z_][a-zA-Z0-9_]*`, case-sensitive. Convention: `snake_case` for functions
and variables, `CamelCase` for types and enum variants. CLRS prints procedure
names in small caps with hyphens (`INSERTION-SORT`); sudo uses `insertion_sort` —
hyphenated identifiers are unambiguous only with mandatory spacing around minus,
which we judged too fragile.

Reserved words:
`and assert break case continue downto else enum expect_trap export false for
func if import in inout match not or record return skip test to true while`.
Reserved type/constructor names: `int float bool text List Map Set Option Result
Some None Ok Err`.

---

## 2. Types

| Type | Meaning |
|---|---|
| `int` | 64-bit two's-complement signed integer. Overflow traps (§4.1). |
| `float` | IEEE 754 binary64, including NaN, ±Inf, −0.0. |
| `bool` | `true` / `false`. |
| `List<T>` | Growable sequence, 0-indexed. |
| `Map<K, V>` | Associative map; `K` must be hashable (§2.2). Iteration order **unspecified**. |
| `Set<T>` | `T` must be hashable. Iteration order **unspecified**. |
| `(T1, T2, …)` | Tuple, 2+ elements. |
| `Option<T>` | `Some(T)` or `None`. A built-in enum. |
| `Result<T, E>` | `Ok(T)` or `Err(E)`. A built-in enum. |
| `func(T1, …) -> R` | Function type. Values are references to top-level functions; **no closures** in v1 (capturing lambdas are a possible future extension). |
| `text` | Alias for `List<int>` where each element is a Unicode scalar value. Identical to `List<int>` in-language; the alias exists so host-boundary adapters map it to native strings (see lockstep.md §5). |
| records, enums | User-defined; §6. |

There is no `null`, no implicit conversions, and no subtyping. `int` and `float`
never mix implicitly; convert explicitly (§4.3).

### 2.1 Value semantics

Every value — including lists, maps, sets, records — is a **value**. Assignment,
argument passing, and returning all behave as a deep copy; mutation through one
variable is never observable through another. Backends may implement this with
copy-on-write, moves, or persistent structures, provided the observable behavior
is identical. In-place mutation across a call boundary uses `inout` (§5.2).

### 2.2 Hashable types

Valid Map keys / Set elements: `int`, `bool`, and tuples, records, enums, and
`List`s composed only of hashable types. `float` is **not** hashable (NaN and −0.0
make cross-language key identity treacherous). `Map`, `Set`, and function types
are not hashable.

### 2.3 Equality

`==` / `!=` are defined for all non-function types and compare **structurally**
(deep). Floats compare by IEEE rules: `NaN != NaN`, `-0.0 == 0.0`. Map and Set
equality is order-insensitive (same entries ⇒ equal). Comparing functions is a
compile error.

Ordering (`< <= > >=`) is defined only for `int` and `float` (IEEE: any
comparison with NaN is false). Lexicographic comparison of lists/text is a
library function, not an operator.

---

## 3. Literals

- Integers: decimal `0`, `42`, `-7` (unary minus), including
  `-9223372036854775808` (the minimum; its magnitude is only writable
  directly after a unary minus). No hex/octal in v1.
- Floats: `1.5`, `0.25`, `2.0` — a `.` with digits on both sides is required.
  No exponent form in v1. `-0.0` is expressible. There are no NaN/Inf literals;
  they arise only from arithmetic (e.g. `0.0 / 0.0`).
- Booleans: `true`, `false`.
- Lists: `[1, 2, 3]`, `[]`. The element type of `[]` comes from inference.
- Tuples: `(a, b)`, `(a, b, c)`. (Parenthesized single expressions are just
  grouping.)
- Text: `"abc"` is sugar for the `List<int>` of the string's Unicode scalar
  values, e.g. `[97, 98, 99]`. Escapes: `\" \\ \n \t \r \u{1F600}`.
- Character: `'a'` is sugar for the `int` scalar value (`97`). Same escapes.
- There are no Map/Set literals in v1; use `Map()` and `Set()` constructors and
  insert. (Brace literals are a candidate for M5.)

---

## 4. Expressions and operators

Precedence, tightest first:

1. postfix: call `f(x)`, index `a[i]`, field/method `x.y`
2. unary `-x`
3. `* / mod`
4. `+ -`
5. `< <= > >= == !=` — **non-associative, no chaining**
6. `not`
7. `and` / `or` (short-circuit) — **no mixing without parentheses**

Arithmetic operators are left-associative. `and`/`or` operate on `bool` only
and short-circuit; `not` on `bool`.

Where mainstream languages *disagree* about precedence, sudo refuses to guess
— parentheses are **required**:

- `not a == b` is a parse error (Python and C parse it differently). Write
  `not (a == b)` or `(not a) == b`. `not`'s operand must be a postfix
  expression, another `not`, or parenthesized — `not m.has(k)` is fine.
- `a or b and c` is a parse error. Write `a or (b and c)` or `(a or b) and c`.
  Chains of one operator (`a and b and c`) are fine.
- `a == b == c` and `a < b < c` are parse errors — comparison operators do
  not chain and are non-associative. Write `(a == b) == c` if you mean it.

### 4.1 Integer arithmetic

`+ - *` and unary `-` **trap** (`Overflow`) when the mathematical result does
not fit in 64-bit two's complement. Silent wraparound was rejected: it turns
natural pseudocode (`factorial(21)`) into identical garbage in every target,
which lockstep testing would then certify as "equivalent". An algorithm that
genuinely needs modular arithmetic writes `mod` explicitly; one that needs
larger integers uses the stdlib `BigInt` (written in sudo itself).

`/` on `int` is **floor division** and `mod` is **floor modulo** (result has the
sign of the divisor), matching CLRS's ⌊a/b⌋ and Python:
`7 / 2 == 3`, `-7 / 2 == -4`, `-7 mod 2 == 1`, `7 mod -2 == -1`.
`x / 0` and `x mod 0` trap `DivByZero`; `(-2^63) / -1` traps `Overflow`.

### 4.2 Float arithmetic

`+ - * /` and unary `-` are IEEE 754 binary64 operations, round-to-nearest-even.
`0.0 / 0.0` is NaN — floats never trap. `mod` is not defined for floats in v1.
No fused multiply-add may be used by any backend (it changes results).

### 4.3 Built-in numeric functions

Only bit-exactly-specifiable operations exist. **No transcendentals** (`sin`,
`exp`, `pow`, …) — libm implementations disagree bitwise; if ever needed they
will be written in sudo itself.

| Call | Type | Semantics |
|---|---|---|
| `float(i)` | int → float | IEEE nearest-even (exact for \|i\| ≤ 2^53). |
| `int(f)` | float → int | Truncate toward zero. Traps `InvalidConvert` on NaN or outside int range. |
| `floor(f)` `ceil(f)` `round(f)` | float → float | IEEE-exact; `round` = nearest, ties **away from zero** (CLRS/school convention; backends must not use bankers' rounding). |
| `sqrt(f)` | float → float | IEEE-correctly-rounded (guaranteed by all targets' hardware/soft-float). `sqrt` of negative is NaN. |
| `abs(x)` | int → int / float → float | `abs(-2^63)` traps `Overflow`. `abs(-0.0) == 0.0`, `abs(NaN)` is NaN. |
| `min(a, b)` `max(a, b)` | int or float | For floats: if either operand is NaN the result is NaN; `min(-0.0, 0.0) == -0.0`, `max(0.0, -0.0) == 0.0`. |

---

## 5. Statements

### 5.1 Variables and assignment

```
x = 0                  // declares x by first assignment; type inferred
x = x + 1              // subsequent assignment; type must match
items: List<int> = []  // annotation, needed only when inference lacks context
a, b = b, a            // tuple assignment; RHS fully evaluated first
```

A variable is declared by its first assignment and scoped to the enclosing
block (function, loop body, etc.); inner blocks see outer variables. Using a
variable before assignment on some path is a compile error. There are no global
mutable variables; module-level `name = constant_expression` bindings are
allowed and immutable.

Assignment targets: variable, `a[i]`, `p.field`, or a tuple of targets.

### 5.2 Functions and `inout`

```
func gcd(a: int, b: int) -> int
    while b != 0
        a, b = b, a mod b
    return a

func insertion_sort(items: inout List<int>)
    ...
```

- Parameter types are required; the return type is required unless the function
  returns nothing (omit `-> R` entirely).
- `inout` parameters: the argument must be a plain variable or a record-field
  path (`x` or `p.field`) — not a list element and not an expression. Changes are
  visible to the caller on return. Passing the same variable to two `inout`
  parameters of one call is a compile error (no aliasing). There is **no
  call-site sigil**: `insertion_sort(A)` mutates `A`, exactly as CLRS reads.
- A call that passes `inout` arguments may appear anywhere an expression can.
  Its effects (the writeback to each `inout` argument) take place immediately
  after the call returns — before anything to its right evaluates — following
  the strict left-to-right evaluation order of §12. The compiler lowers such
  expressions to statement-level temporaries in the shared frontend, so
  generated code in every target shows the sequencing explicitly (a `while`
  condition containing such a call becomes a loop-header re-evaluation; a
  short-circuit operand evaluates only when reached).
- Every path through a value-returning function must `return`; checked at
  compile time.
- Recursion (including mutual) is fully supported. Semantic call depth is
  unbounded; backends map exhaustion to the `StackOverflow` trap where they can
  detect it (see lockstep.md §3).
- `export func …` marks a function as host-facing API: it is what the boundary
  adapters expose, and it must have a concrete (non-generic) signature.
  Non-exported functions are internal (backends may still emit them; hosts
  shouldn't call them).

**Generics.** `func sort<T>(items: inout List<T>, less: func(T, T) -> bool)`.
Type parameters are resolved by whole-program monomorphization; backends never
see a type variable — call sites reference `sort__i64`-style instantiations.
Type arguments are inferred from the arguments and must be concrete at the
call site. Generic functions are called, never referenced as values, and
cannot be `export`ed (host signatures are concrete). Constraints: none needed
(equality/hash requirements are checked structurally at instantiation time).

### 5.3 Control flow

```
if x < 0
    ...
else if x == 0
    ...
else
    ...

while lo <= hi
    ...

for i = 0 to n - 1        // inclusive on both ends, CLRS style
    ...
for i = n - 1 downto 0    // inclusive, descending
    ...
for x in items            // over List elements (in order), Set/Map (unspecified order)
    ...
for k, v in m             // Map iteration yields key, value
    ...
```

- `for i = a to b` iterates `i` over `a, a+1, …, b`; zero iterations if `a > b`
  (`downto`: zero if `a < b`). Bounds are evaluated **once**, before the loop.
  The loop variable is freshly scoped to the loop and may not be assigned inside.
- `for x in c` iterates over a copy of `c` taken at loop entry (value
  semantics: mutating the source during iteration is allowed and does not
  affect the iteration). Over a `Map`/`Set` the order is **unspecified** — the
  single deliberate nondeterminism in sudo. Programs whose observable results
  depend on it are buggy; the lockstep harness exists to catch them.
- `break` exits the innermost loop; `continue` skips to its next iteration
  (re-testing the condition, or advancing the range/iterator). Both are only
  valid inside a loop. (Originally omitted on CLRS-purity grounds; reinstated
  once the compiler grew the machinery anyway and interview-style pseudocode
  clearly wanted them.)
- `skip` — the no-op statement (after Dijkstra's guarded-command language). Its
  main use is a deliberately empty `match` arm (§6.3); blocks may not be empty,
  so `skip` is how "considered, and nothing to do" is written.
- `match` — §6.3.

### 5.4 Assertions and tests

```
test "partition puts pivot in place"
    A = [3, 1, 2]
    quicksort(A)
    assert A == [1, 2, 3]
```

- `test "name"` blocks may appear at top level of any module. They are compiled
  only in test builds. The body is a function of no arguments; `assert expr`
  (expr: `bool`) records a failing assertion and aborts that test (as trap kind
  `AssertFailed`). Test names must be unique within a module.
- `assert` is also allowed inside ordinary functions as a defensive check and
  **always traps** on failure, in every build — anything build-dependent would
  be implementation-defined behavior, which sudo does not have. Library code
  should prefer `Result` over `assert` for real errors.
- `expect_trap KIND` (tests only, and only as a test's **final** statement)
  runs its indented block and passes iff the block traps `KIND`; completing
  without a trap, or trapping a different kind, fails the test with a
  diagnostic. This is how trap behavior — a first-class observable outcome —
  gets lockstep coverage:

  ```
  test "empty pop traps"
      a: List<int> = []
      expect_trap OutOfBounds
          a.pop()
  ```

  The last-statement restriction exists because a trap aborts mid-mutation;
  no observable sudo state may be inspected after one.

---

## 6. User-defined types

### 6.1 Records

```
record Point
    x: int
    y: int

p = Point(1, 2)          // positional, in declaration order
q = Point(x = 1, y = 2)  // or named
p.x = 5                  // field assignment (p is a value; no aliasing)
```

Records are structural bundles with nominal type identity. `==` compares all
fields. No methods, no inheritance, no visibility modifiers.

### 6.2 Enums (tagged unions)

```
enum Tree
    Leaf
    Node(value: int, left: Tree, right: Tree)
```

- Variants have zero or more typed payload fields. Recursive enums are allowed
  (backends box recursive payloads; semantically invisible).
- Construction: `Leaf`, `Node(v, l, r)`. Variant names are module-scoped and may
  be qualified (`Tree.Leaf`) to disambiguate; unqualified use is an error only
  if two enums in scope share a variant name.
- `Option`/`Result` are ordinary enums predeclared by the language.

### 6.3 `match`

```
match t
    case Leaf
        return 0
    case Node(v, l, r)
        return v + tree_sum(l) + tree_sum(r)
```

- Scrutinee: enum, `int`, or `bool`. Patterns: variant with binders
  (`Node(v, l, r)` — always full arity, names bind payloads positionally),
  int/bool literals, and `_` (wildcard).
- Exhaustiveness is **required** and compile-checked; use `case _` for the rest.
  No fallthrough; first matching case wins (relevant only for literal patterns).
  A do-nothing arm is written explicitly with `skip` — visible, intentional
  ignoring, so that adding a variant to a shared enum still flags every match
  that hasn't considered it:

  ```
  match event
      case Added(v)
          total = total + v
      case Removed(v)
          total = total - v
      case Touched
          skip
  ```
- No nested patterns in v1 (`case Some(Node(…))` is out; bind and match again).

---

## 7. Built-in container operations

Read operations take the container by value; mutating operations (marked ✎)
require the receiver to be a mutable path and are the only place methods mutate.
`i` of type `int` everywhere; negative indices are **not** supported.

### List<T>
| Op | Result | Notes |
|---|---|---|
| `a.length` | int | |
| `a[i]` | T | Trap `OutOfBounds` unless `0 <= i < a.length`. |
| `a[i] = v` ✎ | | Same bounds rule. |
| `a.append(v)` ✎ | | Amortized growth; no capacity concept in-language. |
| `a.pop()` ✎ | T | Removes and returns last. Trap `OutOfBounds` if empty. |
| `a.insert(i, v)` ✎ | | Valid for `0 <= i <= a.length`. |
| `a.remove_at(i)` ✎ | T | |
| `a.swap(i, j)` ✎ | | CLRS "exchange A[i] with A[j]". |
| `a + b` | List<T> | Concatenation (new value). |
| `filled(n, v)` | List<T> | `n` copies of value `v`. Trap `InvalidArg` if `n < 0`. |

### Map<K, V>
| Op | Result | Notes |
|---|---|---|
| `Map()` | Map<K, V> | Types from inference/annotation. |
| `m.size` | int | |
| `m[k]` | V | Trap `KeyMissing` if absent. |
| `m[k] = v` ✎ | | Insert or overwrite. |
| `m.get(k)` | Option<V> | The non-trapping lookup. |
| `m.has(k)` | bool | |
| `m.delete(k)` ✎ | bool | `true` if the key was present. |
| `m.keys()` / `m.values()` | List<K> / List<V> | **Unspecified order.** |

### Set<T>
`Set()`, `s.size`, `s.add(v)` ✎ (returns `bool`: newly added?), `s.has(v)`,
`s.remove(v)` ✎ (returns `bool`: was present?), `s.items()` → `List<T>`
(**unspecified order**).

### Option<T> / Result<T, E>
`o.is_some()` / `o.is_none()`; `o.unwrap()` (trap `UnwrapFailed` on `None`);
`o.get_or(default)`. `r.is_ok()` / `r.is_err()`; `r.unwrap()` (trap
`UnwrapFailed` on `Err`); `r.get_or(default)`. Anything richer: use `match`.

### sort
`a.sort()` ✎ — for `List<int>` and `List<float>` in v1: ascending, **stable**;
floats order NaN last, `-0.0` before `0.0` (total order per IEEE totalOrder for
these cases, so every backend agrees). This is the deterministic escape hatch
for Map/Set order-dependence: `keys = m.keys()` then `keys.sort()`.
The generic `sort_by(items, less)` in the sudo-written stdlib
(`stdlib/sorting.sudo`) generalizes this to any element type.

---

## 8. Traps

A trap is a *defined* runtime fault. It aborts the current sudo entry-point call
and is observable: to the host as a language-appropriate error (lockstep.md §5),
to the test harness as the test's outcome. Sudo code cannot catch traps —
recoverable conditions belong in `Option`/`Result`.

Trap kinds (closed set):
`OutOfBounds`, `KeyMissing`, `DivByZero`, `Overflow`, `UnwrapFailed`,
`InvalidConvert`, `InvalidArg`, `AssertFailed`, `StackOverflow`.

For lockstep comparison, traps compare by **kind only** — not location or
message (backends cannot agree on locations). Backends should still attach
source location as diagnostic detail where they can.

---

## 9. Modules and imports

```
import sorting            // sibling file sorting.sudo
...
sorting.quicksort(A)
```

- `import name` loads `name.sudo` from the same directory (search paths are a
  build-tool concern, not a language one). Access is always qualified:
  `module.func`, `module.constant`. No renaming, no wildcard imports, no
  circular imports (compile error).
- Importable in v1: functions (including generic ones — instantiations are
  generated into the defining module) and constants. Module-local records and
  enums cannot yet cross module boundaries; a function whose signature
  mentions one is not callable from outside its module.
- Only `export func` is host-facing.

---

## 10. Grammar (EBNF sketch)

Informal; INDENT/DEDENT are produced by the lexer from leading whitespace.

```
module      = { import } { decl } ;
import      = "import" IDENT NEWLINE ;
decl        = func | record | enum | constbind | testblock ;

func        = [ "export" ] "func" IDENT [ generics ] "(" [ params ] ")"
              [ "->" type ] block ;
params      = param { "," param } ;
param       = IDENT ":" [ "inout" ] type ;
generics    = "<" IDENT { "," IDENT } ">" ;

record      = "record" IDENT INDENT { IDENT ":" type NEWLINE } DEDENT ;
enum        = "enum" IDENT INDENT { variant NEWLINE } DEDENT ;
variant     = IDENT [ "(" IDENT ":" type { "," IDENT ":" type } ")" ] ;
constbind   = IDENT "=" expr NEWLINE ;
testblock   = "test" TEXT block ;

block       = NEWLINE INDENT stmt { stmt } DEDENT ;
stmt        = assign | callstmt | ifstmt | while | forto | forin | matchstmt
            | return | assertstmt | skipstmt | "break" NEWLINE
            | "continue" NEWLINE | expecttrap ;
skipstmt    = "skip" NEWLINE ;
expecttrap  = "expect_trap" IDENT block ;   (* tests only, final statement *)
assign      = target { "," target } "=" expr { "," expr } NEWLINE
            | IDENT ":" type "=" expr NEWLINE ;
target      = IDENT | postfix "[" expr "]" | postfix "." IDENT ;
ifstmt      = "if" expr block { "else" "if" expr block } [ "else" block ] ;
while       = "while" expr block ;
forto       = "for" IDENT "=" expr ( "to" | "downto" ) expr block ;
forin       = "for" IDENT [ "," IDENT ] "in" expr block ;
matchstmt   = "match" expr INDENT { "case" pattern block } DEDENT ;
pattern     = INT | "true" | "false" | "_"
            | [ IDENT "." ] IDENT [ "(" IDENT { "," IDENT } ")" ] ;
return      = "return" [ expr ] NEWLINE ;
assertstmt  = "assert" expr NEWLINE ;

type        = "int" | "float" | "bool" | "text"
            | IDENT [ "." IDENT ]
            | ( "List" | "Set" | "Option" ) "<" type ">"
            | ( "Map" | "Result" ) "<" type "," type ">"
            | "(" type "," type { "," type } ")"
            | "func" "(" [ type { "," type } ] ")" [ "->" type ] ;
```

Expressions follow the precedence table in §4.

---

## 11. Type inference

Local, function-at-a-time (no whole-program inference): parameter and return
types are declared, so inference only flows *within* a body — Hindley–Milner
machinery is unnecessary. First assignment fixes a variable's type; empty
literals (`[]`, `Map()`, `Set()`, `None`) take their type from the first
constraining use or an annotation, and it is a compile error if none exists.
Numeric literals are `int` unless written with a decimal point. There is no
implicit widening — `1 + 2.0` is a compile error (`float(1) + 2.0`).

---

## 12. Determinism contract (summary)

A sudo program's observable behavior — return values of exported functions, test
assertion results, and trap kinds — is a pure function of its inputs, **except**
for the iteration order of `Map`/`Set` (and `keys()`/`values()`/`items()`).
Backends must reproduce everything else bit-exactly: integer overflow traps,
float rounding, evaluation order (strictly left-to-right, arguments before call),
short-circuiting, copy points of value semantics. Two conforming backends may
differ only where a program lets unspecified order leak into its results — and
the lockstep harness treats such divergence as a test failure to be fixed in the
*program* (usually by sorting).
