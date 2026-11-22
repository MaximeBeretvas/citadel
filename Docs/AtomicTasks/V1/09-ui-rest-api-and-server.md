# Task 09: Local Web UI REST API & Axum Server (`citadel ui`)

- **Phase:** 2 (Full V1)
- **Dependencies:** [03-readonly-query-engine.md](./03-readonly-query-engine.md), [05-local-markdown-ingestion.md](./05-local-markdown-ingestion.md)
- **Suggested Commit:** `feat(ui): add Axum localhost server, Origin guard, and /api endpoints`

## Goal
Build the Axum HTTP server (`src/ui/mod.rs`) and JSON REST API (`src/ui/api.rs`) for `citadel ui` per [Docs/SPEC.md](../../SPEC.md#L406-L424), strictly bound to `127.0.0.1` with Origin/Host CSRF protection on mutating endpoints.

## Exact Files
- `src/ui/mod.rs`
- `src/ui/api.rs`
- `src/cli/mod.rs` (wire `citadel ui [--port <port>] [--no-open]`)

## Red-First Test Cases (Write Before Implementation)
1. `ui::tests::health_and_sources_crud_api`: `GET /api/health`, `POST /api/sources` (temp Markdown folder), `GET /api/sources`, `GET /api/sources/:id/pages`, `GET /api/pages/:id`, `GET /api/pages/:id/diff`, `DELETE /api/sources/:id`.
2. `ui::tests::post_query_enforces_readonly_boundary`: `POST /api/query` succeeds on `SELECT` and rejects `DELETE`/`DROP` via `src/db/readonly.rs`.
3. `ui::tests::rejects_cross_origin_mutating_requests`: `POST /api/sources` or `DELETE /api/sources/1` with `Origin: https://evil.example` returns `403 Forbidden`; `Origin: http://127.0.0.1:4317` succeeds.
4. `ui::tests::post_sync_triggers_async_source_refresh`: `POST /api/sources/:id/sync` returns `202 Accepted` (or sync summary) and updates `sync_runs`.

## Acceptance Criteria
- Exposes all 9 endpoints: `/api/health`, `GET/POST /api/sources`, `DELETE /api/sources/:id`, `POST /api/sources/:id/sync`, `GET /api/sources/:id/pages`, `GET /api/pages/:id`, `GET /api/pages/:id/diff`, `POST /api/query`.
- `citadel ui` binds `127.0.0.1:<port>` (default `4317` or ephemeral free port if occupied), prints the URL, and auto-opens browser unless `--no-open` is passed.
- Export `build_router(...)` cleanly so Task 12 (`citadel serve`) can mount `/mcp` onto the same Axum router.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
