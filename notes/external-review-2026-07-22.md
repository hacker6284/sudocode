# External review — bug-hunt starting points (2026-07-22)

Provenance: an external reviewer went through the public repo end-to-end
(README, spec/*, notes/*, conformance, stdlib, examples, full workspace)
and sent Zach this report. Saved verbatim below for a future deep dive /
bug hunt. No confirmed bugs — these are the reviewer's judged
highest-likelihood hiding places.

Coordinator annotations (state as of saving):
- Float edges (item 1): conformance/semantics/floats.sudo exists but the
  reviewer's specific probes (min/max with NaN, -0.0 vs 0.0 in min/max,
  round(2.5) ties, ±1.0/0.0 semantics) should be checked against it —
  likely the single best first step of the hunt.
- C/Zig trap sanitization (item 2): C backend was ASan-clean incl. traps
  at M5 time; UBSan + valgrind on CURRENT generated output (post
  cross-module/monomorphization changes) has not been re-run. Zig has
  never had an equivalent leak audit on abort paths.
- Haskell laziness (item 3): matches our own friction log; the deep-force
  helper exists but StackOverflow catchability and forcing order under
  exotic shapes were flagged "not deeply tested" by the implementing lane.
- BigInt (item 4): agreed prime suspect; lockstep-tested but not
  adversarially (no randomized large-operand cross-checks vs a host
  bignum oracle yet — that's the obvious hunt technique).
- CLI arg parsing / first-error-only reporting (item 5): both real;
  first-error-only is a known UX debt.
- Naming collision with sudocode-ai/sudocode (item 3 under
  recommendations): decision for Zach, unaddressed.

---

**Bug Hunt Report: hacker6284/sudocode (sudo language + sudoc transpiler)**

[Report saved verbatim from the reviewer's message — see conversation of
2026-07-22. Key sections:]

Strengths: modular crate layout; unsafe forbidden; lockstep harness +
executable conformance spec called "gold"; stdlib dogfooding (BigInt in
sudo) and the external Haskell backend as confidence signals; docs
(backend-guide, friction logs) called outstanding; graceful SIGPIPE.
No smoking-gun bugs found in inspected code (CLI, registry/discovery).

High-priority watch areas:
1. Floating-point edges (highest risk): NaN propagation, signed zero,
   min/max with NaN, ties-to-even rounding, fmin/fmax runtime
   differences. Suggested probes: min(NaN,5), max(-0.0,0.0), round(2.5),
   1.0/0.0 vs -1.0/0.0.
2. Traps + mutation in C and Zig: overflow/div-zero must abort without
   UB; mandate ASan+UBSan+valgrind on generated C; Zig abort-path
   cleanup under high recursion.
3. Haskell laziness + trap deferral (friction note soft spots).
4. BigInt: carries/borrows/normalization/floor-div — bugs only surface
   on very large numbers under lockstep.
5. CLI/registry minor: manual arg parsing foot-guns as flags grow;
   path-dedup edge cases (macOS /tmp symlinks, Windows); check reports
   only the first type error.

Medium/lower: possible over-parenthesization (cosmetic); stdlib name
shadowing across more languages; inout+MutBuiltin hoisting in complex
loops as the remaining semantic corner.

Recommendations: run full conformance + sanitizers on float/trap-heavy
generated C/Zig; add targeted float/BigInt conformance cases; consider
clap for the CLI; document exact guaranteed IEEE-754 subset in
spec/lockstep.md; consider a naming disambiguation vs the unrelated
sudocode-ai/sudocode project (sudo-lang / sudocode-lang).

Verdict: "some of the cleanest transpiler infrastructure I've seen";
remaining risk concentrated in hard semantic portability corners, not
implementation sloppiness.
