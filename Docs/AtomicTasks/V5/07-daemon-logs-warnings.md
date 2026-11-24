# Task V5-07: The daemon logs Sync warnings

- **Phase:** V5
- **Dependencies:** V5-06
- **Suggested Commit:** `feat(daemon): log Sync warnings`

## Goal
Each warning in a Sync summary also appears in `daemon.log`, at `warn` level, naming the Source. Today the daemon logs none of them. Done when `sync_warning_lines_name_the_source` passes.

## Exact Files
- `src/daemon/mod.rs`:
  - **New pure function** `sync_warning_lines(summary: &SyncSummary) -> Vec<String>`: one line per warning, each starting with the Source name.
  - **`log_sync_summary`** (line 469): logs each line with `tracing::warn!`. It is also called by the local-folder watcher (line 590), so local-folder Sync warnings get logged too.
- `Docs/SPEC.md` §8.3: the warnings half of the "Planned" sentence becomes current text.
- `Docs/PRD.md` FR-2.7: remove *(Planned: …)*. Do this only if tasks 02-06 have all landed.

## Red-First Test Cases
1. `daemon::tests::sync_warning_lines_name_the_source`: no warnings gives an empty list, and two warnings give two lines containing the Source name.

## Acceptance Criteria
- The complete line of a scheduled Sync is unchanged; any warning lines follow it.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
