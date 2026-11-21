# Task 02: SQLite Schema, WAL Pragmas, FTS5 Triggers & Views

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** [01-project-scaffold.md](./01-project-scaffold.md)
- **Suggested Commit:** `feat(db): add SQLite schema, WAL pragmas, FTS5 triggers, and views`

## Goal
Implement the SQLite + FTS5 storage layer defined in [Docs/SPEC.md](../../SPEC.md#L112-L212): embedded `src/db/schema.sql`, idempotent schema initialization/migrations, RW connection helper with WAL/FK/busy_timeout pragmas, FTS5 sync triggers for `pages_fts` and `sections_fts`, and convenience views `v_sources` and `v_recent_changes`.

## Exact Files
- `src/db/mod.rs`
- `src/db/schema.sql`

## Red-First Test Cases (Write Before Implementation)
1. `db::tests::opens_db_in_wal_mode_with_foreign_keys`: verifies `PRAGMA journal_mode` returns `wal` and `PRAGMA foreign_keys` returns `1`.
2. `db::tests::migrations_are_idempotent`: calling `init_db` twice on the same file succeeds without error.
3. `db::tests::pages_fts_trigger_tracks_insert_update_delete`: inserting, updating, and deleting a row in `pages` keeps `pages_fts MATCH` results exact.
4. `db::tests::sections_fts_trigger_tracks_insert_update_delete`: inserting, updating, and deleting a row in `sections` keeps `sections_fts MATCH` results exact.
5. `db::tests::v_sources_and_v_recent_changes_return_accurate_counts`: deleting a source cascades to `pages`, `sections`, `page_revisions`, `sync_runs`, and FTS indexes.

## Acceptance Criteria
- `src/db/schema.sql` matches [Docs/SPEC.md](../../SPEC.md#L118-L212) column-for-column (`sources`, `pages`, `sections`, `page_revisions`, `sync_runs`, `pages_fts`, `sections_fts`, indexes, triggers, `v_sources`, `v_recent_changes`).
- All FTS5 trigger updates happen atomically inside base-table writes.
- Uses `rusqlite` with `bundled` SQLite so zero system SQLite dependency is required.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
