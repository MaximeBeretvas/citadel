# Task 11: Background Daemon, Scheduler, File Watcher & OS Services

- **Phase:** 2 (Full V1)
- **Dependencies:** [08-web-crawler-spider.md](./08-web-crawler-spider.md)
- **Suggested Commit:** `feat(daemon): add background scheduler, local folder watcher, and launchd/systemd install`

## Goal
Implement `src/daemon/mod.rs` and `src/daemon/service.rs` per [Docs/SPEC.md](../../SPEC.md#L460-L473): single-instance `daemon.pid` lock, 60-second web refresh scheduler (bounded concurrency = 2, 0–30s jitter), `notify` 2-second debounced watcher for local Markdown folders, graceful `SIGINT`/`SIGTERM` shutdown, and `web-vault daemon start|stop|status|install|uninstall` (`launchd` plist on macOS, `systemd --user` unit on Linux).

## Exact Files
- `src/daemon/mod.rs`
- `src/daemon/service.rs`
- `src/cli/mod.rs` (wire `daemon` subcommands)

## Red-First Test Cases (Write Before Implementation)
1. `daemon::tests::selects_only_due_web_sources`: sources with `refresh_interval_secs = 0` or recently synced are skipped; overdue web sources are selected.
2. `daemon::tests::pid_lock_prevents_second_instance`: acquiring a second lock on the same `daemon.pid` fails with a clear "already running" error and releases cleanly on drop.
3. `daemon::tests::local_watcher_reindexes_modified_markdown_file`: modifying a `.md` file in a watched temp folder updates its `pages.content_hash` and `page_revisions` after the debounce window.
4. `service::tests::generates_valid_launchd_plist_and_systemd_unit`: generated plist XML and systemd `.service` unit include the resolved binary path, `WEB_VAULT_HOME`, and restart/KeepAlive settings.

## Acceptance Criteria
- Scheduler and local watcher are exposed as reusable async tasks (`run_scheduler`, `run_watcher`) accepting a `CancellationToken` so Task 12 (`web-vault serve`) can run them alongside the HTTP server.
- Graceful shutdown finishes in-flight SQLite transactions and removes `daemon.pid`.
- Idle daemon memory footprint remains minimal.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
