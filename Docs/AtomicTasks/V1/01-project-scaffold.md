# Task 01: Project Scaffold, Config & CLI Skeleton

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** None
- **Suggested Commit:** `feat(scaffold): initialize Rust crate, config paths, tracing, and CLI skeleton`

## Goal
Create the `web-vault` Rust crate with `Cargo.toml` (including the optional `chrome` feature flag), `src/config.rs` for `WEB_VAULT_HOME` / `--data-dir` / OS default path resolution, `stderr`-only `tracing` initialization, and a compiling `clap` CLI skeleton matching [Docs/SPEC.md](../../SPEC.md#L370-L392).

## Exact Files
- `Cargo.toml`
- `src/main.rs`
- `src/lib.rs`
- `src/config.rs`
- `src/error.rs` (shared `thiserror` error types)
- `.gitignore`

## Red-First Test Cases (Write Before Implementation)
1. `config::tests::resolves_explicit_data_dir_over_env`: `--data-dir` takes precedence over `WEB_VAULT_HOME`.
2. `config::tests::resolves_env_var_when_flag_absent`: `WEB_VAULT_HOME` overrides OS default dir.
3. `config::tests::derives_vault_db_and_pid_paths`: `vault.db`, `daemon.pid`, and `daemon.log` resolve inside the chosen data dir.
4. `cli_parse_smoke`: `clap::Parser::try_parse_from` succeeds for `add`, `list`, `show`, `sync`, `rm`, `query`, `serve`, `mcp`, `ui`, `daemon status`, and `skill install`.

## Acceptance Criteria
- `Cargo.toml` declares `default = []` and `chrome = ["spider/chrome"]` (or prepares feature wiring cleanly without breaking default builds).
- `tracing_subscriber` writes strictly to `std::io::stderr` (never `stdout`).
- Unimplemented subcommands return a clean `anyhow` error rather than panicking (`todo!()` / `unwrap()`).
- Zero `unwrap()` / `expect()` outside `#[cfg(test)]`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
