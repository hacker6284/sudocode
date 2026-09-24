# MegaDreifach

Toy three-megaminx Merkle–Damgård hash. Not for real use. The product name **MegaDreifach** is locked; the puzzle/group stays **megaminx**.

This directory is the published primitive:

| File | Role |
| --- | --- |
| `SPEC.md` | Normative specification (`Hash` / `HashDeck` / `HashDeckBody`, plus `MegaDreifach*` aliases; `*BodyFrom` is the free-start analysis surface) |
| `megadreifach.sudo` | Conformance implementation |
| `kats/megaminx_hash_kats.json` | Published KAT file (pad / IV / `|G|` metadata; research Hash hexes) |

Length extension on bare `Hash` is accepted by design. A green Lean build under `proofs/megadreifach/` is not a security claim. Hand-written Lean is not a proof that this sudo text equals the Lean model.

```sh
sudoc build --target js --tests -o /tmp/megadreifach primitives/hash/megadreifach/megadreifach.sudo
node /tmp/megadreifach/_megadreifach_impl.mjs
```
