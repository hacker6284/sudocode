# sudocode

[![CI](https://github.com/hacker6284/sudocode/actions/workflows/ci.yml/badge.svg)](https://github.com/hacker6284/sudocode/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Targets](https://img.shields.io/badge/targets-py%20%7C%20c%20%7C%20js%20%7C%20rs%20%7C%20swift%20%7C%20zig%20%7C%20hs-8A2BE2)](spec/backend-guide.md)

**The universal library language.**

**sudo** is a tiny programming language that formalizes the pseudocode used in
algorithms classes and programming interviews. You write core logic once in
sudo; the `sudoc` transpiler generates readable, idiomatic source in each
language your codebase uses — currently **Python, C, JavaScript, Rust, Swift,
Zig, and Haskell**. One committed source of truth, many generated
implementations, kept honest by **lockstep tests**: unit tests written in
sudo run in *every* target language, and a harness proves the
implementations behave identically.

```
// binary_search.sudo
export func binary_search(items: List<int>, target: int) -> Option<int>
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

```console
$ sudoc build --target py --target rs binary_search.sudo   # readable Python + Rust
$ sudoc test binary_search.sudo                            # run the tests in ALL six languages, diff outcomes
```

## Why

- **Universal library code.** Two halves of a polyglot codebase share one
  implementation. Edit the sudo source; every language's copy regenerates.
- **Lockstep verification.** The same tests run through every backend.
  Divergence — including accidental reliance on unspecified behavior like map
  iteration order — is a first-class test failure, reported with the exact
  values each language saw:

  ```
  == order_dependent (2 tests; targets: py, c, js, swift, rs, zig)
     DIVERGED  test_diverges_first_key_by_iteration_order
                  py     pass
                  c      trap AssertFailed — line 29: Some(1) != Some(3)
                  ...
     note: the algorithm likely depends on unspecified Map order — sort first.
  ```
- **Readable output.** Generated code looks like code a competent human wrote
  in that language: reviewable, debuggable, steppable. It is a build artifact
  (the sudo source is what you commit) — but it is never a blob.

## Design in one paragraph

Statically typed with aggressive inference, so it reads like pseudocode and
checks like a real language. Fully pure — no I/O, clock, or randomness;
effects live in the host. One `int` (64-bit, overflow **traps** — silent
wraparound is a lie the lockstep harness would otherwise certify), IEEE-754
`float` with only bit-exactly-specifiable operations, value semantics with
`inout`, defined traps instead of exceptions, records + tagged unions +
monomorphized generics, and exactly one deliberate nondeterminism: Map/Set
iteration order — depending on it is a bug, and the harness exists to catch
it. Where mainstream languages disagree on precedence, sudo refuses to guess
and requires parentheses. An arbitrary-precision `BigInt` is available in the
stdlib — written in sudo itself, verified identical across all six targets.

## Quick start

```console
$ cd sudoc && cargo build --release          # builds the `sudoc` CLI
$ cd ..
$ ./sudoc/target/release/sudoc check examples/quicksort.sudo
$ ./sudoc/target/release/sudoc build --target c --target js -o out examples/quicksort.sudo
$ ./sudoc/target/release/sudoc test examples/quicksort.sudo        # lockstep across all installed targets
$ ./sudoc/target/release/sudoc conformance                         # the full cross-backend semantics suite
```

Target toolchains (only needed for the targets you use): Python ≥ 3.10, a C
compiler, Node ≥ 18, Rust, Swift ≥ 6, Zig 0.16.

## Repository layout

| Path | What it is |
|---|---|
| [`spec/language.md`](spec/language.md) | The language specification |
| [`spec/lockstep.md`](spec/lockstep.md) | Transpilation model, lockstep testing, host boundaries |
| [`spec/backend-guide.md`](spec/backend-guide.md) | **How to add a language** — the backend author's handbook |
| [`spec/protocol.md`](spec/protocol.md) | The external backend wire protocol — backends in any language |
| [`sudoc/`](sudoc/) | Rust workspace: compiler frontend, backend SDK, six in-tree backends, harness, CLI |
| [`backends/haskell/`](backends/haskell/) | The Haskell backend, written in Haskell over the wire protocol |
| [`conformance/semantics/`](conformance/semantics/) | The executable spec: every backend must agree on every module here |
| [`stdlib/`](stdlib/) | Libraries written in sudo itself — sorting, strings, BigInt |
| [`examples/`](examples/) | Classic algorithms as living spec anchors |
| [`notes/`](notes/) | Engineering history: design decision log, per-backend friction logs |

## Adding a language

Two ways in, same acceptance bar (`sudoc conformance` green against every
existing backend):

- **In-tree**: implement one small Rust trait (`sudoc_sdk::Backend`) and
  register it in one place.
- **Out-of-tree, in any language**: implement the
  [wire protocol](spec/protocol.md) — a manifest plus an executable that
  reads typed IR as JSON and returns generated files — and plug in with
  `sudoc test --external your-manifest.json`. sudo isn't Rust under the
  hood: the wire format is the same contract the built-in backends are
  CI-verified against (byte-identical output through a serialize→deserialize
  round trip), and the **Haskell backend is written in Haskell**
  ([`backends/haskell/`](backends/haskell/)), proving the protocol from the
  outside.

The [backend guide](spec/backend-guide.md) carries the porting order and the
land-mine catalog learned from building the first seven backends —
evaluation order, precedence portability, value-semantics copy points,
uncatchable traps, and friends. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Status

Working core, pre-1.0: the full language, seven conformant backends (six
in-tree, one external over the wire protocol), lockstep harness with
operand-level divergence diagnostics, host-boundary adapters for Python and
C, and a sudo-written stdlib. Spec and IR may still change before a
stability commitment (the wire protocol is versioned; changes bump it).

## License

[MIT](LICENSE) © Zach Mills
