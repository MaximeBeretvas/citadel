# Task V3-03: Cached 24h update-check & non-blocking CLI notice

- **Phase:** V3
- **Dependencies:** none
- **Suggested Commit:** `feat(update): add cached 24h update-check and non-blocking CLI notice`

## Goal
Add lightweight, offline-safe automatic update awareness backed by `<CITADEL_HOME>/update-check.json` (24h TTL) and surface a single-line update notice on `stderr` in interactive CLI commands without ever slowing startup or polluting `citadel mcp` `stdio` or `--json` output.

## Exact Files
- `Cargo.toml`: add `semver = "1"`.
- `src/config.rs`:
  - Add `pub const NO_UPDATE_CHECK_ENV: &str = "CITADEL_NO_UPDATE_CHECK";`.
  - Add `pub no_update_check: bool` field to `VaultConfig` (parsed via `parse_flag`).
  - Add `pub fn update_check_path(&self) -> PathBuf` (`self.data_dir.join("update-check.json")`).
- `src/update/check.rs` (new):
  - `UpdateCheckCache { checked_at: String, latest_version: String, release_url: String }`.
  - `UpdateStatus { current_version: String, latest_version: Option<String>, update_available: bool, release_url: Option<String> }`.
  - `read_cached_status(path: &Path, current_version: &str, now_secs: i64) -> (UpdateStatus, bool /* is_stale */)`: synchronous read (0 ms network cost), `semver` comparison, 24h TTL check; corrupt/missing file treated as stale with `update_available = false`.
  - `refresh_cache(api_url: &str, path: &Path) -> Result<UpdateCheckCache>`: fetches GitHub Releases latest JSON (`tag_name`, `html_url`) with short timeout (3s) and `User-Agent: citadel/<version>`, writes atomically to `update-check.json`.
  - `spawn_background_refresh(path: PathBuf)`: detached non-blocking refresh that swallows network/offline errors silently.
- `src/update/mod.rs` (new, minimal): expose `pub mod check;`.
- `src/lib.rs`: register `pub mod update;`.
- `src/cli/mod.rs`:
  - After interactive CLI commands (excluding `Commands::Mcp`, `Commands::Update`, `--json` outputs, non-TTY `stderr`, or `config.no_update_check`), read cached status, print 1-line `stderr` notice if `update_available`, and trigger non-blocking refresh if stale.

## Red-First Test Cases
1. `update::check::tests::semver_comparison_detects_newer_release_only`: `0.2.0 > 0.1.0` -> true; `0.1.0 == 0.1.0` -> false; `0.0.9 < 0.1.0` -> false; strips leading `v`.
2. `update::check::tests::cache_ttl_marks_fresh_within_24h_and_stale_after_24h`.
3. `update::check::tests::missing_or_corrupt_cache_degrades_silently_as_stale`.
4. `update::check::tests::refresh_cache_writes_atomic_json_from_mock_github_endpoint`: local loopback Axum/TCP server returning `{ "tag_name": "v0.2.0", "html_url": "https://..." }`.
5. `config::tests::parses_no_update_check_env_and_derives_cache_path`.
6. Verify `tests/mcp_stdio.rs` receives zero update-notice bytes on `stdout` even when `update-check.json` reports a newer version.

## Acceptance Criteria
- `citadel mcp` `stdout` stays 100% pure JSON-RPC even with a newer version cached in `update-check.json`.
- `CITADEL_NO_UPDATE_CHECK=1` suppresses both notice output and background refresh.
- Offline/unreachable GitHub endpoint never causes a CLI error or startup delay.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
