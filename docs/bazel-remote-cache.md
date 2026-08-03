# Bazel remote cache (BuildBuddy)

Keeps CI builds fast and, with Build-without-the-Bytes, keeps local build
outputs off disk. **Toolchain downloads (rustc, etc.) are always local** — the
remote cache does not avoid those.

## One-time setup (you)

1. Sign up (free OSS tier) at https://app.buildbuddy.io and create an org.
2. Copy the API key from Settings → API keys.
3. Local dev (optional, read-through cache + smaller disk):
   `bazel build --config=remote --remote_header=x-buildbuddy-api-key=YOUR_KEY //...`
   or put the key in `~/.bazelrc` (NOT the repo `.bazelrc`):
   `build:remote --remote_header=x-buildbuddy-api-key=YOUR_KEY`
4. CI: add the key as the `BUILDBUDDY_API_KEY` GitHub Actions secret.

## Config (already in repo `.bazelrc`)

- `--config=remote` → `--remote_cache=grpcs://remote.buildbuddy.io` +
  `--remote_download_minimal` (Build-without-the-Bytes: output tree stays remote).
- `--config=ci` → `remote` + `clippy` + `--remote_upload_local_results`.

## Security: fork-PR cache poisoning

Only trusted runs (main-branch pushes) get cache **write**. Fork PRs run
read-only (or no key), so a malicious PR cannot poison the cache. In CI, gate
the `--remote_upload_local_results` + key on `github.event_name == 'push'`.
