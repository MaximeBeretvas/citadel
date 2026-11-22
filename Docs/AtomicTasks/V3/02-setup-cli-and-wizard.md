# Task V3-02: Interactive setup wizard & `citadel setup` CLI

- **Phase:** V3
- **Dependencies:** V3-01
- **Suggested Commit:** `feat(cli): add interactive citadel setup wizard and non-interactive flags`

## Goal
Wire `citadel setup` into the CLI with a guided `cliclack` terminal wizard (for interactive TTYs) and full non-interactive flag support (for scripts/CI), including automatic detection of whether a local `citadel serve` HTTP MCP endpoint is reachable.

## Exact Files
- `Cargo.toml`: add `cliclack = "0.3"` (or latest compatible `0.3.x`).
- `src/setup/mod.rs`:
  - `detect_default_transport(probe_url: &str) -> McpTransport`: fast HTTP probe (short timeout, e.g., 500 ms) to `http://127.0.0.1:4317/mcp` (or custom URL); defaults to `Http` if reachable, else `Stdio`.
  - `run_interactive_wizard(cwd, home, probe_url)` using `cliclack`:
    1. Multi-select target agent(s): Claude Code, Antigravity (`agy`), Codex.
    2. Select scope: Local (current folder) or Global.
    3. Select components: Skill + MCP (default), Skill only, MCP only.
    4. Confirm MCP transport (`stdio` vs `http`, pre-selecting the auto-detected default; allow editing HTTP URL if `http` chosen).
    5. Show summary of planned files (marking existing `citadel` entries), prompt for confirmation (and overwrite confirmation when conflicts exist), apply plan, and print completion summary.
  - `run_non_interactive(opts)` for flag-driven runs (refuses overwrite unless `--force`).
- `src/cli/mod.rs`:
  - Add `Commands::Setup(SetupArgs)` with `--agent <claude|agy|codex>...`, `--scope <local|global>`, `--transport <stdio|http>`, `--url <http-mcp-url>`, `--only <skill|mcp|both>`, `--force`, `--yes`.
  - Dispatch to interactive wizard when stdin/stderr is a TTY and required flags are omitted; otherwise validate flags and run non-interactively.

## Red-First Test Cases
1. `setup::tests::detect_transport_returns_http_when_endpoint_reachable_and_stdio_when_closed`: bind an ephemeral Axum/TCP listener to verify `Http` detection, and an unused port to verify `Stdio` fallback.
2. `setup::tests::non_interactive_setup_creates_claude_and_codex_local_configs`: invoke non-interactive setup into a tempdir and verify `./.claude/skills/citadel/SKILL.md`, `./.mcp.json`, `./.agents/skills/citadel/SKILL.md`, and `./.codex/config.toml`.
3. `setup::tests::non_interactive_setup_fails_on_conflict_without_force_and_succeeds_with_force`.
4. `cli::tests::parses_setup_subcommand_flags`: clap parsing test for `--agent claude --agent agy --scope local --transport stdio --yes`.

## Acceptance Criteria
- `citadel setup --help` documents all flags clearly.
- Non-interactive `citadel setup --agent claude --scope local --yes` exits 0 on fresh folder, exits non-zero on re-run without `--force`, and exits 0 with `--force`.
- Existing `citadel skill install` continues to work unchanged.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
