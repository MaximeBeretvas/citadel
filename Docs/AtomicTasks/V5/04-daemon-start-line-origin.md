# Task V5-04: Log the Origin when a scheduled web Sync starts

- **Phase:** V5
- **Dependencies:** none
- **Suggested Commit:** `feat(daemon): log the Origin when a scheduled web Sync starts`

## Goal
The daemon's "starting scheduled web sync" line carries `origin=<url>`, so `sync_log_report.py` knows each Sync's host from its first line. Done when a foreground daemon's log shows `origin=` on that line.

## Exact Files
- `src/daemon/mod.rs`:
  - Clone `source.origin` into `origin` before line 449, where the Source moves into `claim_record`. This is the same pattern as `name` at line 448.
  - Add `origin = %origin` to the `tracing::info!` at line 460.

## Red-First Test Cases
1. The start-line fixtures in task 01's test 1 use exactly this format: `starting scheduled web sync source=<name> origin=<url>`. The daemon has no unit test for `tracing` output. Compare the fixture with the line the daemon writes (see Acceptance).

## Acceptance Criteria
- In a foreground daemon on a scratch data directory (`citadel daemon start --foreground`, with stderr captured), the start line of a scheduled web Sync reads `starting scheduled web sync source=<name> origin=<url>`.
- The complete and failed lines are unchanged.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
