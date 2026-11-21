# Task 03: Read-Only SQLite Query Engine & Safety Guardrails

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** [02-sqlite-schema-and-fts5.md](./02-sqlite-schema-and-fts5.md)
- **Suggested Commit:** `feat(db): add read-only SQL query executor with authorizer and resource caps`

## Goal
Build `src/db/readonly.rs`—the hardened read-only SQL execution boundary shared by MCP `query`, CLI `query`, and UI `/api/query` per [Docs/SPEC.md](../../SPEC.md#L214-L225).

## Exact Files
- `src/db/readonly.rs`
- `src/db/mod.rs` (export `readonly`)

## Red-First Test Cases (Write Before Implementation)
1. `readonly::tests::allows_select_and_fts5_bm25_snippet`: valid `SELECT` with `sections_fts MATCH`, `bm25()`, and `snippet()` returns typed JSON columns/rows + `elapsed_ms`.
2. `readonly::tests::blocks_all_write_and_ddl_statements`: `INSERT`, `UPDATE`, `DELETE`, `DROP`, `ALTER`, `CREATE`, `REPLACE`, `VACUUM` are rejected.
3. `readonly::tests::blocks_attach_detach_and_pragma`: `ATTACH DATABASE`, `DETACH`, and `PRAGMA` mutations are denied by the SQLite authorizer.
4. `readonly::tests::rejects_multi_statement_sql`: `"SELECT 1; SELECT 2"` and `"SELECT 1; DROP TABLE sources"` are rejected before execution.
5. `readonly::tests::enforces_row_limit_and_truncation_flag`: default limit 50, hard max 200; sets `truncated: true` when more rows exist.
6. `readonly::tests::enforces_100kb_payload_cap`: huge text results stop cleanly at 100 KB with `truncated: true` and a hint to query `sections` or `snippet()`.
7. `readonly::tests::aborts_slow_query_on_timeout`: recursive CTE / heavy query is aborted by `progress_handler` within the configured timeout (use a short test timeout like 100 ms to keep tests fast).

## Acceptance Criteria
- Opens DB with `OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX` + `PRAGMA query_only = ON`.
- Installs an explicit SQLite authorizer allowlist (`Select`, `Read`, `Function`, `Recursive`).
- Returns structured `QueryResult { columns, rows, row_count, truncated, elapsed_ms }` and clear, actionable error messages on failure.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
