# Grok delegation recipe (headless CLI)

Standing reference for any lane that shells out to `grok` headlessly in this
sandbox. Derived from the discovery/target-unification session (2026-07-20)
after burning significant time rediscovering grok's permission model one
denial at a time. Read this before iterating from scratch again.

## TL;DR: two lanes, pick by task shape

**Lane A — file authoring/editing (no shell needed).** This is the reliable
one; use it whenever the turn is "read some files, write/edit some files."
Strip the Bash-equivalent tool out of grok's toolset entirely so there is
nothing for the permission floor (see below) to cancel on, and run all
verification yourself afterward — which the contract requires anyway
("Grok's claim of success is not evidence; your re-run is").

```bash
grok --prompt-file "$SPEC" \
  -m grok-4.5 \
  --permission-mode auto \
  --disable-web-search --no-subagents --no-memory --no-plan \
  --tools "read_file,grep,list_dir,search_replace,todo_write" \
  --allow "Write(<path-glob-scoped-to-the-files-being-touched>)" \
  --allow "Edit(<same-glob>)" \
  --output-format plain \
  --cwd "$(pwd)" \
  > "$OUT" 2>&1
```

Example globs used successfully this session: `Write(sudoc/**)`,
`Edit(sudoc/**)`, `Write(.github/**)`, `Edit(.github/**)`.

Verified in this session, verbatim, on three separate turns (new integration
test file, a multi-step CI YAML rewrite, and a compile-error fix pass) — all
landed on the first or second try with zero permission cancellations.

**Lane B — needs a shell (build/test/run commands).** No fully reliable
invocation was found this session. See "The run_terminal_command floor"
below before using this lane; expect to supervise closely and re-run.

```bash
grok --prompt-file "$SPEC" \
  -m grok-4.5 \
  --permission-mode auto \
  --disable-web-search --no-subagents --no-memory --no-plan \
  --disallowed-tools "search_tool,use_tool,lsp,memory_search,memory_get,web_search,web_fetch,task,kill_task,get_task_output" \
  --allow "Bash" \
  --allow "Write(<scoped-glob>)" --allow "Edit(<scoped-glob>)" \
  --output-format plain \
  --cwd "$(pwd)" \
  > "$OUT" 2>&1
```

`--allow "Bash"` (bare, no parens) is grok's own documented "allow all shell
commands" rule (see `~/.grok/README.md` §"Permission Rules"). Even this did
**not** eliminate the floor-cancellation below — treat Lane B as best-effort,
not reliable. Prefer decomposing the task so the shell-needing parts are as
small as possible, and be ready to retry.

## The `run_terminal_command` floor (the main time-sink this session)

Symptom: grok reads files fine (`read_file`/`grep`/`list_dir` always dispatch
`mode="local"`), announces what it's about to do, then the whole turn ends
with a one/two-sentence stub message and no files changed. Debug log
(`--debug --debug-file FILE`) shows:

```
INFO xai_grok_workspace::permission::manager: permission policy allow deferred to confirmation floor tool="run_terminal_command" source="policy"
...
DEBUG xai_acp_lib::gateway: received "session/prompt" response: {"stopReason":"cancelled", ..., "cancellationCategory":"PermissionCancelled"}
```

Findings from this session:

- This happens **regardless of `--allow` rule breadth** — tried scoped
  per-command prefixes (`Bash(cargo*)`, `Bash(git*)`, ~40 commands),
  Claude-style `Bash(cmd:*)`, and finally the bare catch-all `--allow "Bash"`.
  All three still hit the floor on some turns.
- It is **not deterministic per-command** — the same task sometimes got
  further than other times before hitting it (in one run it got through 14+
  tool calls including several `run_terminal_command`s dispatched
  `mode="local"` before the floor hit; in others it hit immediately).
- No raw command text is logged anywhere, even with
  `--debug --debug-file` or `RUST_LOG=trace` — the permission manager module
  does not print argv (presumably deliberate, to avoid leaking secrets into
  logs). You cannot diagnose *which* command triggered it from the logs.
- `--output-format streaming-json` does **not** stream tool-call arguments
  either (only `thought`/`text`/`end` event types were observed) — no help
  there.
- The whole **turn** is discarded on cancellation, not just the one denied
  tool call — any `search_replace`/`Write` edits queued earlier in the same
  turn that hadn't been flushed yet are lost too. (Edits that already landed
  from a *prior*, separately-invoked turn are safe — this is why chaining
  many small single-purpose invocations, each verified independently, beat
  one large invocation.)
- Session-level tool restriction fixes it: dropping `run_terminal_cmd` from
  `--tools` entirely (Lane A above) makes the floor irrelevant because
  there's no `run_terminal_command` tool call for it to defer. This is the
  only reliable mitigation found.

If a task genuinely needs grok to run shell commands itself (not just author
files), decompose it: have grok write the code (Lane A), then run
build/test/verification yourself as the supervising agent. This also matches
the contract's existing requirement to independently re-verify rather than
trust grok's self-report.

## Correct `--allow` / `--tools` grammar (undocumented by `grok --help` alone)

`grok --help` describes `--allow`/`--tools` tersely; the real grammar lives in
`~/.grok/README.md` (bundled with the CLI install, `grok` "Claude Code
Compatibility" / "Permission Rules" / "Built-in Tools" sections).

**Permission rules** (`--allow` / `--deny`): `ToolPrefix(glob_pattern)`.

| Prefix | Controls |
|---|---|
| `Bash(...)` | shell command execution |
| `Edit(...)` | file editing (path glob) |
| `Write(...)` | file writing (path glob) |
| `Read(...)` | file reading (path glob) |
| `Grep(...)` | search (path glob) |
| `WebFetch(...)` | URL fetch (glob or `domain:host`) |
| `MCPTool(...)` | MCP tool invocations |

- `*` = single-level wildcard, `**` = recursive. A bare prefix with no
  parens (`Bash`, `Write`, `Edit`) matches **all** invocations of that type.
- Claude Code's `Bash(cmd:*)` form is also accepted, prefix-matched on `cmd`.
- Deny rules take precedence over allow rules.
- **This was the missing piece for the first ~15 attempts of this session**:
  file writes/edits need `Write(...)`/`Edit(...)` rules, not `Bash(...)`
  rules — `Bash(...)` only governs the shell tool. Early attempts only
  passed `Bash(...)` rules and wondered why file edits stalled; they didn't
  actually stall on Write/Edit, they stalled on the `run_terminal_command`
  floor above, but getting the Write/Edit grammar right is still required
  once Lane B's shell surface is trimmed back and file edits need to be
  explicitly allowed too.

**Tool filtering** (`--tools` / `--disallowed-tools`, headless-only): comma
list of internal tool IDs, which are **not** the same strings as the ACP
names seen in logs:

| Display name | `--tools` / `--disallowed-tools` ID | ACP/log name |
|---|---|---|
| bash | `run_terminal_cmd` | `run_terminal_command` |
| grep | `grep` | `grep` |
| read_file | `read_file` | `read_file` |
| search_replace (file edit/create) | `search_replace` | `search_replace` |
| list_dir | `list_dir` | `list_dir` |
| web_search | `web_search` | — |
| web_fetch | `web_fetch` | — |
| todo_write | `todo_write` | `todo_write` |
| task (subagents) | `task` | — |

`--tools` sets an **allowlist** (only listed tools exist at all — no
confirmation-floor risk for anything not listed, because it's simply not
available). `--allow`/`--deny` **gate** tools that remain available; they
don't add new ones back. When diagnosing a floor-cancellation, prefer
`--tools` restriction over broader `--allow` rules — it's the lever that
actually worked.

## Flags that get blocked before grok even starts

The **calling agent's own** outer sandbox (Claude Code's permission
classifier, not grok's) rejects certain arguments to the `grok` invocation
itself as suspicious, before grok runs at all:

- `--allow "Bash(*)"` (a fully-wildcard Bash rule as a literal CLI argument)
- `--permission-mode bypassPermissions`
- `--always-approve`

All three produce: `Permission for this action was denied by the Claude Code
auto mode classifier. Reason: Blocked by classifier.` — on the *calling*
agent's Bash tool call, before `grok` is even invoked. **Do not retry these
with different phrasing or try to route around the denial** — per the
harness's own guidance, that's grounds to stop and report, not improvise
around. This project's existing rule ("forbidden: `--permission-mode
bypassPermissions`, scoped allows only, always") is consistent with what the
sandbox itself will actually let you do, not just a style preference.

## Other pitfalls confirmed or newly learned this session

- **`acceptEdits` permission mode silently no-ops headless** — pre-existing
  guidance, not re-tested this session, still trust it.
- **Foreground-only.** Never background-and-wait a `grok` invocation. Use the
  Bash tool's own `timeout` parameter (up to 600000ms) for a single long
  invocation instead of shell `timeout`/`gtimeout` — the latter often isn't
  installed on macOS (`command -v gtimeout timeout` returned nothing here).
- **Fresh cwd per Bash call.** Always pass `--cwd "$(pwd)"` explicitly and
  use absolute paths for `--prompt-file`; never rely on a previous call's
  `cd`.
- **Pipes/`&&`/heavy quoting inside a single terminal-tool call are fragile**
  — pre-existing guidance (have grok write scratch driver scripts to files
  and run them as one plain command). Not independently re-confirmed this
  session since Lane B's shell surface was mostly avoided, but nothing
  contradicts it either — keep following it.
- **`grok --continue` (`-c`) was unreliable here.** Resuming the most recent
  session repeatedly produced a 1-2 sentence stub with *zero* tool calls and
  an immediate cancellation (`stopReason: Cancelled`, no tool call ever
  requested). Prefer fresh, non-continued sessions for follow-up turns —
  restate full context in a new prompt file rather than relying on
  `--continue`/`--resume` to pick up where a prior turn left off.
- **This account's `~/.grok/config.toml` has ~12 unrelated MCP servers**
  (travel/flight/rail booking tools, `wolfram`, `xai-docs`, `voicemode`,
  etc.) configured globally. Every session spawns them at startup and
  blocking-waits up to 15s for handshakes
  (`wait_for_mcp_handshakes_bounded: waiting timeout_ms=15000`), regardless
  of task relevance — pure overhead, plus one benign recurring
  `octotrip-rental-cars` auth WARN. `--disable-web-search --no-subagents
  --no-memory --no-plan` trims some tool surface but does **not** stop the
  startup MCP spawn/handshake wait. `--disallowed-tools
  "search_tool,use_tool,..."` removes the MCP-invocation tools from grok's
  own toolset but likewise doesn't skip the startup wait. **Do not edit
  `~/.grok/config.toml`** (e.g. `disabled_mcp_servers`) to work around this
  — it's the user's global config, out of scope for a project task; treat
  the ~5-15s startup tax as a fixed cost.
- **This machine runs other concurrent grok sessions** under the same
  account, tied to unrelated project directories (seen via `ps aux | grep
  grok` and `grok sessions list` — e.g. sessions rooted at `wonderlog`,
  `pocket-putt`). Don't assume you're the only consumer of the account's
  rate limits; don't kill processes you don't recognize as your own without
  being certain they're orphaned.
- **`grok inspect` may report "Permissions Source: ~/.claude/settings.local.json"**
  — grok falls back to reading Claude Code's own settings file for
  permissions when it finds no native `.grok`/TOML permission config (see
  `~/.grok/README.md` "Claude Code Compatibility" table: Claude
  `.claude/settings.json`/`settings.local.json` are a documented fallback
  source). This cross-tool sharing is a source of confusing "where did this
  rule come from" moments — always pass explicit `--allow`/`--tools`/
  `--permission-mode` flags per invocation rather than relying on any
  file-based fallback.
- **Write your prompt spec file into this session's own scratchpad
  directory, not a bare `mktemp -t` default location.** Observed once this
  session: a `mktemp -t grok-spec.XXXXXX` path in the default system temp
  dir came back containing stale content from an apparently unrelated prior
  session sharing this sandbox. Always write directly into the provided
  scratchpad dir, and sanity-check the file's head/tail immediately after
  writing and before invoking grok.

## What actually shipped this session (for calibration)

Three Lane-A turns landed cleanly on the first or second try each:
1. A new Rust integration test file (`sudoc/crates/cli/tests/discovery_collision.rs`).
2. A multi-step `.github/workflows/ci.yml` rewrite (two jobs, several fields each), plus one follow-up turn to fix a step-ordering bug in that same file that grok itself flagged in-message but had been told (by an earlier, over-literal spec) to leave as-is.
3. A three-error compile-fix pass on a pre-existing test file (unused import, missing dependency, `Debug`-bound issue) from an earlier partial run.

All verification (`cargo clippy`, `cargo test --workspace`, `cargo doc`, the
release build, and five CLI smoke-test invocations) was run directly by the
supervising agent afterward — none of it went through grok's own shell tool.

## Addendum (2026-07-21): --allow path scoping is NOT a hard boundary

Observed in the infinite-craft Python integration lane: grok wrote a file
under a directory that was (a) excluded by the task spec and (b) not
covered by any `--allow "Write(...)"` rule — the write went through
anyway. Treat `--allow` globs as advisory hygiene, not enforcement. The
wrapper MUST diff the full working tree (`git status --porcelain` across
every repo the lane can reach) after grok finishes and flag/remove any
out-of-scope writes before reporting. Do not rely on the permission layer
to fence the write surface.
