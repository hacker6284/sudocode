# sudocode

**sudo** is a minimal programming language that formalizes the pseudocode used in
algorithms classes and programming interviews (CLRS-flavored). You write core logic
once in sudo; the `sudoc` transpiler generates readable, idiomatic source in each
language your codebase uses. One committed source of truth, many generated
implementations — kept honest by **lockstep tests**: unit tests written in sudo run
in *every* target language, and a harness proves the implementations behave
identically.

```
// binary_search.sudo
func binary_search(items: List<int>, target: int) -> Option<int>
    lo = 0
    hi = items.length - 1
    while lo <= hi
        mid = (lo + hi) / 2
        if items[mid] == target
            return Some(mid)
        else if items[mid] < target
            lo = mid + 1
        else
            hi = mid - 1
    return None

test "finds an element"
    assert binary_search([1, 3, 5, 7], 5) == Some(2)

test "misses cleanly"
    assert binary_search([1, 3, 5, 7], 4) == None
```

```
$ sudoc build --target py --target c binary_search.sudo
$ sudoc test binary_search.sudo        # runs the tests in Python AND C, diffs outcomes
```

## Why

- **Universal library code.** Two halves of a polyglot codebase can share one
  implementation. Edit the sudo source; every language's copy regenerates.
- **Lockstep verification.** The same tests run through every backend. Divergence —
  including accidental reliance on unspecified behavior like map iteration order —
  is a first-class test failure.
- **Readable output.** Generated code looks like code a competent human wrote in
  that language: reviewable, debuggable, steppable. It is a build artifact (the sudo
  source is what you commit), but it is never a blob.

## Design pillars

- Statically typed with aggressive inference — reads like pseudocode, rigorous underneath.
- Fully pure: no I/O, clock, RNG, or globals. Effects live in the host language.
- One integer type: `int` is i64 with defined two's-complement wraparound.
- `float` is IEEE 754 binary64; only IEEE-exact operations are provided.
- Value semantics everywhere + `inout` parameters; aliasing is unobservable.
- Runtime faults (out-of-bounds, division by zero) are *defined traps*, observable
  at the host boundary and comparable across languages.
- Map/Set iteration order is unspecified — depending on it is a bug, and the
  lockstep harness is designed to catch it.
- Records, tagged unions with `match`, and generics via whole-program
  monomorphization. No vtables, no boxing, tiny per-target runtime.

See [spec/language.md](spec/language.md) for the language and
[spec/lockstep.md](spec/lockstep.md) for transpilation, testing, and host-boundary
mapping.

## Repository layout

```
spec/            language spec, lockstep/boundary spec, backend author's guide
sudoc/           Rust workspace: compiler, backend SDK, backends, harness, CLI
stdlib/          libraries written in sudo itself (sorting, strings, BigInt)
examples/        classic algorithms — living spec anchors
conformance/     semantics/ = the executable spec: every backend must agree
                 on every module here (sudoc conformance --target X)
```

## Status

Pre-alpha, core complete: the language (records, enums, generics, imports,
break/continue, expect_trap, overflow-trapping i64), the Python and C
reference backends with host-boundary adapters, the lockstep harness with
operand-level divergence diagnostics, a sudo-written stdlib (generic
sorting, strings, arbitrary-precision BigInt), and the backend SDK with an
executable conformance suite. Next: JS, Rust, Swift, and Zig backends,
built on the SDK (see spec/backend-guide.md).

## Naming

The language is **sudo** (`.sudo` files), the compiler is **sudoc**, the project is
**sudocode**. Yes, we know about `sudo(8)`; the CLI is `sudoc`, so your shell will
forgive you.
