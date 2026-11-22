# Task V3-01: Setup agent resolver & safe JSON/TOML MCP config mergers

- **Phase:** V3
- **Dependencies:** none
- **Suggested Commit:** `feat(setup): add per-agent path resolver and safe JSON/TOML MCP mergers`

## Goal
Implement the pure, filesystem-testable core of `citadel setup` in `src/setup/agents.rs` and `src/setup/merge.rs`: resolve per-agent skill and MCP config paths for **Claude Code**, **Antigravity (`agy`)**, and **Codex** (local & global), and safely merge `citadel` MCP entries into JSON and TOML files without clobbering unrelated entries or TOML comments.

## Exact Files
- `Cargo.toml`: add `toml_edit = "0.22"` (or latest compatible `0.22.x`).
- `src/setup/agents.rs` (new):
  - `AgentKind` (`Claude`, `Agy`, `Codex`), `SetupScope` (`Local`, `Global`), `SetupComponents` (`Both`, `SkillOnly`, `McpOnly`), `McpTransport` (`Stdio`, `Http(String)`).
  - `PlannedAction` / `SetupPlan` structs listing exact target paths, whether each target already exists, and whether a `citadel` entry would be overwritten.
  - Path resolution using explicit `(cwd: &Path, home: Option<&Path>)` matching `Docs/SPEC.md` §6:
    - Claude local: `./.claude/skills/citadel/SKILL.md` + `./.mcp.json`
    - Claude global: `~/.claude/skills/citadel` (symlink -> `~/.agents/skills/citadel`) + `~/.claude.json`
    - Antigravity local: `./.agents/skills/citadel/SKILL.md` + `./.agents/plugins/citadel/plugin.json` + `./.agents/plugins/citadel/mcp_config.json`
    - Antigravity global: `~/.gemini/config/skills/citadel` (symlink -> `~/.agents/skills/citadel`) + `~/.gemini/config/mcp_config.json`
    - Codex local: `./.agents/skills/citadel/SKILL.md` + `./.codex/config.toml`
    - Codex global: `~/.agents/skills/citadel/SKILL.md` + `~/.codex/config.toml`
- `src/setup/merge.rs` (new):
  - `merge_json_mcp_config(existing: Option<&str>, transport: &McpTransport, include_http_type: bool, force: bool)`: inserts/updates `mcpServers.citadel` via `serde_json`, preserving all other top-level keys and MCP servers; returns an error if `mcpServers.citadel` already exists and `force == false`.
  - `merge_toml_mcp_config(existing: Option<&str>, transport: &McpTransport, force: bool)`: inserts/updates `[mcp_servers.citadel]` via `toml_edit`, preserving comments, formatting, and unrelated `[mcp_servers.*]` tables; returns an error if `[mcp_servers.citadel]` exists and `force == false`.
  - Helper to scaffold minimal `plugin.json` for local Antigravity plugin (`.agents/plugins/citadel/plugin.json`).
- `src/setup/mod.rs` (new, minimal): re-export `agents` and `merge` plus `apply_plan(&SetupPlan, force: bool)` executor.
- `src/lib.rs`: register `pub mod setup;`.

## Red-First Test Cases
1. `setup::merge::tests::json_merge_preserves_other_servers_and_keys`: existing `.mcp.json` with `other-server` and unrelated keys retains both after adding `citadel`.
2. `setup::merge::tests::json_merge_refuses_overwrite_without_force_and_updates_with_force`.
3. `setup::merge::tests::toml_merge_preserves_comments_and_other_servers`: existing `config.toml` with comments and `[mcp_servers.other]` keeps comments intact after adding `[mcp_servers.citadel]`.
4. `setup::merge::tests::toml_merge_refuses_overwrite_without_force_and_updates_with_force`.
5. `setup::agents::tests::resolves_all_six_agent_scope_combinations`: verifies exact skill + MCP paths for `(Claude|Agy|Codex) x (Local|Global)` using temp `cwd` and `home`.
6. `setup::tests::apply_plan_scaffolds_agy_local_plugin_and_claude_global_symlink`: end-to-end tempdir apply verifying files, symlinks, and JSON/TOML contents for both `stdio` and `http` transports.

## Acceptance Criteria
- No `unwrap()`/`expect()` outside `#[cfg(test)]`.
- Applying a plan in a tempdir never touches the real `$HOME`.
- All 6 agent/scope combinations match `Docs/SPEC.md` §6 exactly.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
