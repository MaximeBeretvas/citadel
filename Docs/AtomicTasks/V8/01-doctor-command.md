# Task V8-01: Add `citadel doctor`

- **Phase:** V8
- **Dependencies:** V7-01
- **Suggested Commit:** `feat(cli): add citadel doctor command`

## Goal
`citadel doctor [--json]` prints one read-only report: is the CLI and daemon up to date, what is running, and where the skill and MCP entry are set up. Design: [doctor-command](../../superpowers/specs/2026-11-25-doctor-command-design.md). Plan: [doctor-command plan](../../superpowers/plans/2026-11-25-doctor-command.md).

Done when:
- each check is a pure function with unit tests over injected inputs;
- `tests/doctor.rs` passes: a fresh home creates no data dir or config file, a broken `.mcp.json` exits 1, all four sections print, and no update notice shows;
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `cargo build --no-default-features && cargo test --no-default-features` pass.

## Interface
```text
citadel doctor [--json]        exit 0 unless a check fails (warnings exit 0), exit 1 otherwise
run_doctor(config, &DoctorOpts) -> Report
Report { checks: [Check { section, name, status, detail, hint }] }
```

## Exact Files
- `src/doctor/{mod,report,version,running,setup}.rs` (new): `run_doctor` and the checks.
- `src/daemon/info.rs` (new): `daemon.json`, written by the daemon while it holds the lock.
- `src/cli/mod.rs`: `Commands::Doctor`, excluded from the automatic update notice.
- `tests/doctor.rs` (new): end-to-end checks against a temp home and project.
- `Docs/SPEC.md` §1 and §6, `README.md`: document the command and `daemon.json`.

## Acceptance Criteria
- Doctor creates nothing: not the data dir, `vault.db`, `update-check.json` or any agent config.
- Every network or process probe has a short timeout.
- Every `!` and `✗` row carries a hint naming the fixing command; doctor never runs it.
