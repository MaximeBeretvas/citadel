# AGENTS.md — web-vault

## Context Pointers
Read the authoritative doc before acting on its branch; never duplicate its details here:
- [PRD.md](./PRD.md) — product scope, user journeys, functional & non-functional requirements.
- [SPEC.md](./SPEC.md) — Rust crate stack, module layout, SQLite + FTS5 schema, MCP `query`/`manage` contracts, CLI, embedded UI, and Docker/native deployment.
- `TASKS.md` *(created in AtomicTasks)* — ordered atomic tasks and acceptance criteria.

## Execution Loop
Follow `AtomicTasks -> LLMCode -> Checks -> HumanAudit` for every change:

1. **Red first (TDD):** Write a failing unit or integration test for the task or bug before writing production code.
2. **One atomic task:** Scope edits to a single task from `TASKS.md`; keep diffs tight and commit with one Conventional Commit once green.
3. **Checks gate (all must pass before `HumanAudit`):**
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
   When `Dockerfile` or `docker-compose.yml` changes, also verify:
   ```bash
   docker build -t web-vault:check .
   ```
   If any check fails, fix the root cause and re-run the full gate.
4. **HumanAudit handoff:** Summarize changed files, exact command results, and any risks or assumptions; wait for human review before merging.

## Engineering Invariants
- **Error handling:** Propagate errors with `Result`, `thiserror` (library modules), and `anyhow` (CLI/top-level); reserve `unwrap()` and `expect()` strictly for `#[cfg(test)]`.
- **MCP `stdio` purity:** Send all `tracing` logs to `stderr` or `daemon.log`; keep `stdout` strictly reserved for JSON-RPC frames in `web-vault mcp`.
- **SQLite read-only boundary:** Execute every MCP `query` through `src/db/readonly.rs` (`SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only = ON`, authorizer allowlist, 5s timeout, row/byte caps). Re-run the read-only safety tests after any `src/db/` change.
- **MCP `manage` validation:** Enforce `http(s)://` URL checks with SSRF blocking (loopback/RFC1918/link-local) and directory canonicalization for local Markdown folders. Re-run validation tests after any `src/mcp/` or `src/ingest/` change.
- **FTS5 consistency:** Maintain `pages_fts` and `sections_fts` via SQLite triggers inside the same transaction as `pages`/`sections` writes. Keep `src/db/schema.sql` and `SPEC.md` in sync.
- **Diff hygiene:** Normalize line endings and strip volatile frontmatter timestamps before SHA-256 hashing; mark missing/failing pages `stale`/`error` rather than silently deleting them.
- **Zero-runtime footprint:** Keep the binary self-contained (`rusqlite` bundled, `rust-embed` offline UI assets, zero external CDNs, minimal idle RAM in `serve`/`daemon`).

## Lessons
*(Updated via `Telemetry -> learn -> AgentRules`: add one crisp, positive rule per real check failure, audit finding, or runtime bug; prune entries once obsolete.)*
- *(none yet)*
