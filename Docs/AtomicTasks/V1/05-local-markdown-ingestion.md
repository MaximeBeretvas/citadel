# Task 05: Local Markdown Ingestion & Core CLI Commands

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** [02-sqlite-schema-and-fts5.md](./02-sqlite-schema-and-fts5.md), [03-readonly-query-engine.md](./03-readonly-query-engine.md), [04-normalize-diff-and-sections.md](./04-normalize-diff-and-sections.md)
- **Suggested Commit:** `feat(ingest,cli): implement local Markdown folder sync and core CLI commands`

## Goal
Implement local Markdown folder ingestion (`src/ingest/local.rs` + `src/ingest/mod.rs`) and wire up the core CLI commands (`add`, `list`, `show`, `sync`, `rm`, `query`) so engineers can index local Markdown folders (e.g., converted books), re-sync with diff/stale tracking, and query from the terminal.

## Exact Files
- `src/ingest/local.rs`
- `src/ingest/mod.rs` (source CRUD + sync orchestration + `sync_runs` recording)
- `src/cli/mod.rs` (command handlers + `table`/`json`/`csv` formatters)
- `src/main.rs` (wire CLI handlers)

## Red-First Test Cases (Write Before Implementation)
1. `local::tests::ingests_nested_markdown_and_ignores_git_hidden_dirs`: indexes `*.md` / `*.markdown`, skips `.git/` and `node_modules/`, stores relative paths as `uri`.
2. `local::tests::resync_skips_unchanged_and_records_diff_on_change`: modifying 1 file updates only that page, writes `page_revisions`, refreshes `sections`, and leaves untouched files counted in `pages_unchanged`.
3. `local::tests::marks_deleted_file_stale_and_prunes_only_with_flag`: removing a file marks `status = 'stale'` by default; `--prune` deletes stale rows.
4. `local::tests::canonicalizes_path_and_rejects_non_directory`: rejects missing paths or non-directory files cleanly.
5. `cli::tests::end_to_end_add_sync_query_rm`: runs `add <tempdir>`, `list --json`, `query "SELECT ... FROM sections_fts ..."`, and `rm <name> --yes`.

## Acceptance Criteria
- Each page upsert + section rebuild + `page_revisions` upsert runs inside a single SQLite transaction.
- `web-vault add <path>` auto-detects local directory vs `http(s)://` URL (web sync returns a clear "not yet implemented until Task 08" or registers with `--no-sync` until Task 08 lands).
- `web-vault query "<sql>"` routes through `src/db/readonly.rs` and supports `--format table|json|csv`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
