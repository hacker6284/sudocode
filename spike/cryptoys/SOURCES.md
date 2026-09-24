# Vendored cryptoys sources (read-only fixtures)

These files are copies of published cryptoys algorithm sources for the
Lean-emitter validation spike. They are not rewritten.

| Path | Origin |
|---|---|
| hash/megadreifach/megadreifach.sudo | hacker6284/cryptoys `main` @ 628932cbcc3be606d942e330729e497a99ab853a |
| hash/megadreifach/kats/megaminx_hash_kats.json | same |
| hash/scramble/scramble.sudo | same |
| cipher/twodeck/twodeck.sudo | same (main still uses TwoDeck name) |
| cipher/doubledeal/doubledeal.sudo | hacker6284/cryptoys `cursor/doubledeal-rename-63b7` @ fbe223f4d6614bb1bac0c38295b8e302e7726a82 (PR #11) |

DoubleDeal is a rename of TwoDeck; the .sudo bodies are expected to be
equivalent aside from product naming. The spike emits DoubleDeal as the
cipher under test, and notes TwoDeck on main for KAT/proof correspondence.
