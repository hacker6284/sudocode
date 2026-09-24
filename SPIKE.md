# SPIKE: can `backends/lean/` serve cryptoys?

**Verdict: CONDITIONAL GO**

- **GO** to *feed* cryptoys `proofs/` with executable Lean 4.14: emit → `lake build` → TAP, and to lockstep-compare digests against sudo self-tests / the Python reference backend.
- **NO-GO** to *replace* the hand-written Lean models under `cryptoys/proofs/*/lean/`. Those are algebraic correctness developments (zero `sorry`, Fin-based). This emitter produces fuel-total `Except Trap` programs. This spike does **not** claim sudo↔Lean semantic-equivalence proofs.
- **NO-GO** for `sudoc emit-ir --require terminates` on these algorithms today: every public API is a `RefusedExport` because `while` has no decreases measure. That is **not** how this backend is registered (empty `predicates`, full peer). Cryptoys' own proofs README still describes a future “total-fragment Lean emitter”; what #5 ships is a fuel-total lowering of the full language.

Parent PR #5 (`cursor/lean-external-backend-36b2`) is **not** merge-ready as a lockstep peer. This spike does not register Lean in `ALL_BACKENDS`.

| | |
|---|---|
| Spike branch | `cursor/lean-cryptoys-spike-4644` |
| Branched from | `cursor/lean-external-backend-36b2` @ `c72e2da` (sudocode PR #5 head) |
| Lean | 4.14.0 (`leanprover/lean4:v4.14.0`, commit `410fab728470`) |
| Lake | 5.0.0-410fab7 |
| Emitter | `python3 backends/lean/emit.py` (protocol 4) |
| sudoc | `cargo build --release -p sudoc-cli` from this checkout |

## Per-algorithm results

Sources (vendored under `spike/cryptoys/`, not rewritten):

| Algorithm | Origin |
|---|---|
| `megadreifach.sudo` + `kats/megaminx_hash_kats.json` | cryptoys `main` @ `628932cb` |
| `scramble.sudo` | same |
| `doubledeal.sudo` | cryptoys `cursor/doubledeal-rename-63b7` @ `fbe223f4` (PR #11) |
| `twodeck.sudo` | cryptoys `main` (body-identical to DoubleDeal aside from `//` comments) |

Profile = **full peer** (no `--require`). That is what `sudo_external_backend` on #5 actually runs.

| Algorithm | emit? | `lake build`? | TAP / KATs? | First hard blocker on #5 as-is |
|---|---|---|---|---|
| MegaDreifach | **yes** | **no** → **yes** after `_fs` fix | **11/11 TAP**; IV-COOK12 hex matches published KAT; research `Hash` hexes do **not** (but Lean == Python) | **(e) emitter** — `for s` shadows Flow binder `s` |
| DoubleDeal (TwoDeck rename) | **yes** | **yes** (even without the fix) | **12/12 TAP**, including published encrypt/decrypt decks | none for full-peer emit/build/run |
| Scramble | **yes** | **yes** (even without the fix) | **15/15 TAP**, including v1/v2 digest + facelet vectors | none for full-peer emit/build/run |

`--require terminates` (not the registered profile):

| Algorithm | emit-ir | First `RefusedExport` |
|---|---|---|
| MegaDreifach | fail | `pad_message` → `trim` — `while has no decreases measure` (`megadreifach.sudo:318`) |
| DoubleDeal | fail | `encrypt` → `passkey` — `while has no decreases measure` (`doubledeal.sudo:251`) |
| Scramble | fail | `evaluate` — `while has no decreases measure` (`scramble.sudo:576`); also `update` → `apply_ready` |

Classifier for the MegaDreifach `lake build` failure on unmodified #5:

| Class | Hit? |
|---|---|
| (a) sudo feature the emitter refuses/mishandles | Only under `--require terminates` (`while`). Full peer accepts `while`/`for` via `SudoRt.natIter`. |
| (b) Lean totality/runtime gaps | No. Fuel lowering compiled and ran. No `partial` / `sorry`. |
| (c) stdlib gaps (regex/bigint) | No. Cryptoys does not `import std.regex` / `std.bigint`. MegaDreifach pastes its own `record BigInt`. |
| (d) size/complexity | No. ~3–5k lines of generated Lean; `lake build` 10–30s. |
| (e) other | **Yes — identifier collision.** |

## Exact commands

```bash
# sudoc (this checkout)
cargo build --release -p sudoc-cli --manifest-path sudoc/Cargo.toml

# Lean (elan)
elan toolchain install leanprover/lean4:v4.14.0
elan default leanprover/lean4:v4.14.0

# emit one algorithm (full peer, same envelope as rules_sudo/private/lockstep.bzl)
python3 spike/emit_one.py spike/cryptoys/hash/megadreifach/megadreifach.sudo \
  --out /tmp/mega
# internals:
#   sudoc emit-ir -I stdlib -o modules.json FILE
#   wrap {"protocol":4,"cmd":"emit","entry":"<stem>","with_tests":true,"modules":...}
#   python3 backends/lean/emit.py   # cwd = backends/lean so it finds SudoRt.lean

cd /tmp/mega/files && lake build && ./.lake/build/bin/megadreifach_test

# totality profile (refuses all three cryptoys entries)
sudoc emit-ir --require terminates -I stdlib FILE   # exit 1, RefusedExport
```

`emit-ir` does **not** take `--tests`. Tests live in the IR modules; `with_tests: true` in the envelope asks the emitter for a TAP exe.

## Blocker: `for s` vs Flow binder `s`

#5's `emit_for_range` hardcodes `| .brk s` / `| .cont s` for the body's `Flow` payload. MegaDreifach names the loop index `s` (`compose`, `inverse`, `abs_reorient`, …):

```271:273:spike/cryptoys/hash/megadreifach/megadreifach.sudo
    for s = 0 to 19
        cp.append(h.cp[g.cp[s]])
        co.append((h.co[g.cp[s]] + g.co[s]) mod 3)
```

Generated Lean then does (pre-fix):

```lean
| .brk s => match s with | (cp, co) => pure (SudoRt.Flow.brk (s, (cp, co)))
| .cont s => match s with | (cp, co) => do
    if s == _toV then ...
    let i' ← SudoRt.addI s (1 : Int)
```

After the match, `s` is the carried state, not the index.

- When carried state is `Array Int × Array Int` (compose): **type error**, `lake build` fails. First errors at `Megadreifach.lean:531` (`Prod.mk s` has type `Array Int × Array Int`, expected `Int`) and `abs_reorient` (`Flow.brk s` has type `Unit`, expected `Int`).
- When carried state is also `Int` (tiny fixture `spike/fixtures/for_s_shadow.sudo`): **builds, computes the wrong answer** (`sum_s(3)` → `3` not `6`).

This spike renames those binders to `_fs` in `backends/lean/emit.py`. After that:

- fixture TAP `1/1`
- MegaDreifach `lake build` + TAP `11/11`
- DoubleDeal / Scramble still `12/12` and `15/15`

Cherry-pick that hunk onto #5; do not treat the rest of this branch as backend-complete.

## KATs vs published vectors

**Scramble.** The sudo tests *are* the published digests (empty / `A7` / `hello` / `cube` / `a`, v1 and v2) plus trap cases. Lean TAP `15/15`.

**DoubleDeal / TwoDeck.** TAP `decrypt undoes encrypt` uses the same message/key/cipher as `proofs/twodeck/vectors/twodeck_vectors.json` vector `encrypt_published`. cryptoys records `sudo_sha256 = bbca24a3…` for `twodeck.sudo`; that matches the vendored main file, and DoubleDeal is comment-identical. Lean TAP `12/12`.

**MegaDreifach.**

| Check | Result |
|---|---|
| sudo TAP pad / φ / IV-COOK12 / aliases / traps | 11/11 in Lean |
| published `iv_cook12_digest_hex` | **match** `0000021aeb876eb76dd8bf833457a2c02613e55656963e02d8dfedb5aa` |
| research `Hash` hexes in `megaminx_hash_kats.json` (`empty`, `short_abc`) | Lean ≠ JSON |
| Lean `v_Hash` vs `sudoc build --target py` `Hash` | **equal** (`empty` = `037ef527…`, `abc` = `025959c0…`) |

`megadreifach.sudo` line 6: *“Research Hash hexes in kats/megaminx_hash_kats.json are not asserted here.”* The JSON is stale relative to current sudo; that is a cryptoys bookkeeping issue, not an emitter semantic bug.

## What cryptoys still needs before it can rely on this

Emitter / sudocode (this repo):

1. **Land the `_fs` Flow-binder rename** (or equivalent freshening) so `for s` is not a silent or hard failure.
2. Keep the #5 registration story: empty `predicates`, **not** in `ALL_BACKENDS` until the unfinished peer suite (regex/bigint, examples, Bazel lockstep) is green.
3. Do **not** advertise `--require terminates` as the cryptoys path until either (i) the algorithms grow decreases measures / bounded `for`, or (ii) cryptoys explicitly accepts fuel-total `while` as the executable model.
4. Optional later: a real total-fragment story (the future item in `cryptoys/proofs/README.md`). Fuel-`natIter` is not that.
5. Optional: emit names that are closer to the sudo source (`Hash` is `v_Hash` because `Hash` collides) if cryptoys wants to `import` generated modules next to hand proofs.

Cryptoys (consumer, not done here):

1. Treat emitted Lean as a **vector / TAP oracle**, not as a replacement for `proofs/megadreifach/lean` / `proofs/twodeck/lean`.
2. Refresh `megaminx_hash_kats.json` Hash hexes to the current sudo (Python and Lean already agree).
3. If they want `--require terminates`, rewrite `while` loops (`trim`, `passkey`, `evaluate`, …) with a measure or a `for` over a known bound. Do **not** paper over the emitter hole by renaming `s` in MegaDreifach — that would hide the silent-wrong case.

## What this spike is not

- Not a merge of sudocode PR #5.
- Not a sudo↔Lean equivalence proof.
- Not a claim that generated Lean is bit-security evidence.
- Not a rewrite of cryptoys algorithms (the only extra sudo is `spike/fixtures/for_s_shadow.sudo`).
