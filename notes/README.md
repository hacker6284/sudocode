# Engineering notes

Unpolished-by-design history of how sudocode got built. Kept in-tree because
the reasoning behind decisions is as load-bearing as the decisions.

- `decision-log.md` — design and process decisions in chronological order,
  including reversals (integer overflow went from wrap to trap; `break`/
  `continue` went from banned to supported) and the honest record of what
  multi-agent development got wrong and right.
- `friction-<lang>.md` — one per backend: every place the backend guide, the
  SDK, or the target language surprised the implementer. These logs are the
  raw material from which the guide's land-mine catalog was distilled; new
  backends are expected to add their own.
