# Task V2-04: Uniform failure recording & per-source `sync_all`

- **Phase:** V2
- **Dependencies:** [02-origin-policy.md](./02-origin-policy.md), [03-sync-claim.md](./03-sync-claim.md)
- **Suggested Commit:** `fix(sources): record every sync failure and sync all sources independently`

## Goal
- **Failure recording:** every failed Sync leaves the same trace, whatever the entry point (CLI, MCP, UI, scheduler, watcher, add-then-sync): an error Sync run plus `sources.status = 'error'` and `last_error`.
- **`sync_all`:** stops aborting on the first failure.

## Exact Files
- `src/sources/mod.rs`:
  - One failure path inside the Sync, replacing `record_pre_walk_error`. It covers origin-policy rejections, missing folders, and crawler setup errors raised before `start_sync_run`.
  - `sync_all` returns `Vec<SourceSyncResult>` (Source name + `Result<SyncOutcome>`).
- `src/cli/mod.rs`: `sync --all` prints every result and exits non-zero if any Source failed.
- `src/mcp/mod.rs`: `manage sync` (all) returns per-source results.

## Red-First Test Cases
1. `sources::tests::missing_local_folder_records_error_run`: via `sync`, and via the watcher path.
2. `sources::tests::rejected_private_origin_records_error_run`
3. `sources::tests::add_with_sync_failure_records_error_run`
4. `sources::tests::sync_all_continues_after_failure`: three Sources with the middle one broken; the other two are `Synced`.
5. `cli::tests::sync_all_exits_nonzero_when_any_source_fails`

## Acceptance Criteria
- `record_pre_walk_error` is deleted.
- `show_source` shows the error for every failure scenario above.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
