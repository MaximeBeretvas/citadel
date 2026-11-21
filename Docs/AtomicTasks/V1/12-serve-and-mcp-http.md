# Task 12: Unified `web-vault serve` & MCP Streamable HTTP (`/mcp`)

- **Phase:** 2 (Full V1)
- **Dependencies:** [06-mcp-stdio-query-and-manage.md](./06-mcp-stdio-query-and-manage.md), [10-embedded-web-ui-assets.md](./10-embedded-web-ui-assets.md), [11-daemon-scheduler-and-watcher.md](./11-daemon-scheduler-and-watcher.md)
- **Suggested Commit:** `feat(serve): add unified serve mode with MCP Streamable HTTP endpoint`

## Goal
Implement `web-vault serve [--bind 0.0.0.0:4317]`—the default container entrypoint combining the background scheduler, local folder watcher, embedded Web UI (`/`), REST API (`/api/*`), and **MCP Streamable HTTP** (`/mcp`) in one process per [Docs/SPEC.md](../../SPEC.md#L11-L13) and [Docs/SPEC.md](../../SPEC.md#L275-L279).

## Exact Files
- `src/mcp/http.rs` (Axum handler mounting `POST/GET /mcp` over the shared `mcp` dispatcher)
- `src/mcp/mod.rs` (refactor JSON-RPC request dispatch so `stdio` and HTTP share 100% of tool logic)
- `src/serve.rs` (starts scheduler + watcher + Axum server with unified graceful shutdown)
- `src/cli/mod.rs` (wire `web-vault serve`)

## Red-First Test Cases (Write Before Implementation)
1. `mcp::http::tests::post_mcp_initialize_and_tools_list`: `POST /mcp` returns valid JSON-RPC `initialize` and `tools/list` (`["query", "manage"]`).
2. `mcp::http::tests::post_mcp_query_and_manage_roundtrip`: `POST /mcp` executes `manage(add)` and `query(SELECT ...)` identically to `stdio` mode and enforces the same read-only & SSRF guards.
3. `serve::tests::serves_ui_api_health_and_mcp_on_single_port`: starting `serve` on an ephemeral port serves `/`, `/api/health`, and `/mcp` simultaneously and shuts down cleanly on cancellation.

## Acceptance Criteria
- Zero duplicated tool logic between `web-vault mcp` (`stdio`) and `/mcp` (Streamable HTTP).
- `serve` binds `0.0.0.0:4317` by default (overrideable via `--bind` or `WEB_VAULT_BIND`).
- Graceful `SIGINT`/`SIGTERM` stops the HTTP server, scheduler, and folder watcher cleanly.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
