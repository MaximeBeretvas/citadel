# Task 06: MCP `stdio` Server (`query` + `manage` Tools)

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** [03-readonly-query-engine.md](./03-readonly-query-engine.md), [05-local-markdown-ingestion.md](./05-local-markdown-ingestion.md)
- **Suggested Commit:** `feat(mcp): implement stdio MCP server with query and manage tools`

## Goal
Implement `citadel mcp` over `stdio` JSON-RPC 2.0 exposing strictly the **2 composable tools** defined in [Docs/SPEC.md](../../SPEC.md#L272-L361): `query(sql, limit?)` (read-only) and `manage(action, ...)` (`add`, `sync`, `update`, `remove`) with SSRF and directory canonicalization guardrails.

## Exact Files
- `src/mcp/mod.rs`
- `src/mcp/prompt.rs` (concise tool descriptions with schema & FTS5 recipes)
- `src/mcp/validate.rs` (URL scheme + SSRF IP/host guard + local path canonicalization)
- `src/main.rs` (wire `citadel mcp`)

## Red-First Test Cases (Write Before Implementation)
1. `validate::tests::blocks_ssrf_targets`: rejects `file://`, `ftp://`, `http://127.0.0.1`, `http://localhost`, `http://169.254.169.254`, `http://10.0.0.1`, `http://192.168.1.1`, `http://[::1]`, and allows public `https://docs.example.com`.
2. `mcp::tests::initialize_and_tools_list_exposes_exactly_two_tools`: JSON-RPC `initialize` + `tools/list` returns exactly `["query", "manage"]` with valid JSON schemas.
3. `mcp::tests::query_tool_executes_fts5_and_blocks_writes`: `tools/call` `query` returns JSON rows on `SELECT` and returns `isError: true` on `DELETE`/`PRAGMA`.
4. `mcp::tests::manage_tool_add_sync_update_remove_local_source`: `tools/call` `manage` adds a temp Markdown folder, verifies it is queryable via `query`, updates tags, and removes it.
5. `mcp::tests::stdio_stdout_contains_only_valid_jsonrpc_frames`: spawning `citadel mcp` binary with `RUST_LOG=debug` produces zero non-JSON lines on `stdout`.

## Acceptance Criteria
- Strictly 2 tools exposed: `query` and `manage`.
- `query` executes exclusively through `src/db/readonly.rs`.
- `manage` validates all URLs against SSRF ranges and canonicalizes local directory paths before ingestion.
- All logs route strictly to `stderr`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
