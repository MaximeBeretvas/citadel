# Task V2-01: Source lifecycle module (move + async-only)

- **Phase:** V2
- **Dependencies:** none
- **Suggested Commit:** `refactor(sources): extract async-only source lifecycle module`

## Goal
Create `src/sources/` holding the **Source lifecycle** (add, update, remove, sync) behind one async-only `SourceLifecycle` interface. **Behaviour is unchanged**: this is a pure move plus de-duplication. `ingest/` keeps only the local walker, the web crawler, normalisation and section splitting.

## Exact Files
- `src/sources/mod.rs` (new): `SourceLifecycle::{new, add, update, remove, sync, sync_all}`, plus source registration, slugging, origin classification and limit validation moved from `ingest/mod.rs`.
- `src/ingest/mod.rs`:
  - Delete `add_source`, `add_source_async`, `sync_sources`, `sync_sources_async` and `run_async_blocking`.
  - Keep the read functions (`list_sources`, `show_source`) here for now.
- `src/cli/mod.rs`: build a Tokio runtime for `add` and `sync` (as it already does for `serve`/`ui`) and call `SourceLifecycle`.
- `src/mcp/mod.rs`, `src/mcp/http.rs`:
  - `handle_jsonrpc_payload` becomes async.
  - The stdio loop runs inside a runtime.
  - The HTTP handler drops `spawn_blocking`.
- **Blocking work:** the module runs the local folder walk and all rusqlite calls inside `tokio::task::spawn_blocking`, so none of them run on runtime worker threads. MCP `query` (the 5s read-only executor) likewise runs inside `spawn_blocking` once dispatch is async. Futures returned by `SourceLifecycle` must be `Send`, because the UI and scheduler spawn them.
- `src/ui/api.rs`, `src/daemon/mod.rs`: call `SourceLifecycle` instead of `ingest::*` or the direct `sync_web_source` / `sync_local_source` calls.
- `src/lib.rs`: register `pub mod sources`.
- `CONTEXT.md` is already updated. `Docs/SPEC.md` §2: update the module layout.

## Red-First Test Cases
1. `sources::tests::add_local_then_sync_reports_summary`: add a tempdir Markdown folder with `sync_now = true`; the summary counts match.
2. `sources::tests::add_web_then_sync_via_loopback_fixture`: port the `web.rs` `add_source_async` fixture tests to `SourceLifecycle`.
3. `sources::tests::update_and_remove_roundtrip`.
4. Delete the tests that only exercised the sync/async twins, and move the remaining add/sync tests from `ingest/web.rs` and `ingest/mod.rs` here. Crawler-specific tests stay in `web.rs`.

## Acceptance Criteria
- `grep -rn "add_source_async\|sync_sources_async\|run_async_blocking" src` returns nothing.
- No production caller outside `src/sources/` calls `sync_web_source` or `sync_local_source`.
- `tests/mcp_stdio.rs` still passes unchanged.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
