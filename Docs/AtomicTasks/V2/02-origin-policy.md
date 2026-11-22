# Task V2-02: Origin policy everywhere

- **Phase:** V2
- **Dependencies:** [01-source-lifecycle-module.md](./01-source-lifecycle-module.md)
- **Suggested Commit:** `fix(sources): enforce private-origin policy on every entry point`

## Goal
Enforce the Private-origin check (loopback, RFC1918, link-local, CGNAT, multicast, local hostnames) inside `SourceLifecycle`, so CLI, UI, MCP, scheduler and watcher all get one rule. Implements SPEC §5 "unless explicitly enabled via config".

## Exact Files
- `src/sources/origin.rs` (new): origin classification and the Private-origin rule, moved from `mcp/validate.rs` and `ingest::classify_origin`. One parse and one rule.
- `src/config.rs`: add `allow_private_origins: bool`, read from `CITADEL_ALLOW_PRIVATE_ORIGINS` (`1`/`true`). Default `false`.
- `src/sources/mod.rs`: apply the check on `add` and at the start of every Sync (rejects Private origins stored before this change).
- `src/mcp/validate.rs`:
  - Delete `validate_web_url` and the `validate_local_dir` pass-through.
  - Check whether the `--no-sync` error rewrite at `mcp/mod.rs:336` is dead (no error string appears to contain `--no-sync`); if it is, delete it.
  - Delete `looks_like_url`.
- `AGENTS.md`: rewrite the "MCP `manage` validation" invariant as "Source lifecycle validation", enforced in `src/sources/` for every entry point.
- `src/ingest/web.rs`:
  - Keep the redirect guard.
  - Reuse the `sources::origin` IP rules instead of `is_loopback_or_private_host`'s weaker copy, so CGNAT and multicast are covered.
- `Docs/SPEC.md` §5/§6: document the env var.
- `README.md`: document the env var.

## Red-First Test Cases
1. `sources::origin::tests::*`: port the `validate.rs` table tests.
2. `sources::tests::add_rejects_private_origin_by_default`.
3. `sources::tests::add_allows_private_origin_when_enabled`.
4. `sources::tests::sync_rejects_stored_private_origin_when_disabled`: insert the row directly, then Sync; expect `Validation`.
5. One test per entry point:
   - `cli` `add http://127.0.0.1/`
   - UI `POST /api/sources` with `http://10.0.0.1/`
   - MCP `manage add` with `http://169.254.169.254/`

   Each must fail with a validation error.

## Acceptance Criteria
- Exactly one implementation of the Private-origin rule in `src/`.
- Existing loopback-fixture tests pass by enabling `allow_private_origins` on their `VaultConfig`.
- The rewritten AGENTS.md invariant matches the code.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
