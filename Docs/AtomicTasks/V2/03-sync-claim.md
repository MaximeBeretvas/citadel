# Task V2-03: Sync claim

- **Phase:** V2
- **Dependencies:** [01-source-lifecycle-module.md](./01-source-lifecycle-module.md)
- **Suggested Commit:** `feat(sources): add database sync claim across processes`
- **Decision record:** [ADR-0001](../../adr/0001-database-sync-claim.md)

## Goal
At most one Sync per Source at a time, across every process sharing the vault. `remove` respects the claim too.

## Exact Files
- `src/db/schema.sql` + `Docs/SPEC.md` §3: add the `sync_claims` table (`source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE, pid INTEGER NOT NULL, instance_token TEXT NOT NULL, claimed_at TEXT NOT NULL`). Keep the two files in sync.
- `src/sources/claim.rs` (new):
  - Take the claim: `INSERT`.
  - Release: `DELETE`.
  - Instance token: a random token generated once per process, stored with the pid.
  - Takeover: an `UPDATE` guarded by the old pid and token, allowed when any of these holds:
    - the holder pid is not running;
    - the pid equals our own but the token is not ours (a previous run reused the pid, e.g. pid 1 in the container);
    - `claimed_at` is older than the hard age limit (6h).
  - Clock and pid-liveness sit behind a crate-private hook for tests.
  - A takeover or release also resets a stuck `sources.status = 'syncing'`.
- `src/sources/mod.rs`:
  - `sync` returns `SyncOutcome::{Synced, AlreadySyncing}`.
  - `claim(name) -> Option<ClaimedSync>` plus `ClaimedSync::run(prune)`. `ClaimedSync` releases on finish, with `Drop` as the fallback.
  - `remove` takes the claim and returns `Removal::{Removed, AlreadySyncing}`.
- `src/ui/api.rs`: `POST /api/sources/{id}/sync` claims before replying. It returns 202 when claimed and 409 when already syncing, then moves the claim into the spawned task.
- `src/daemon/mod.rs`:
  - Scheduler: delete the in-flight set and the `status != 'syncing'` due-query filter. On `AlreadySyncing`, skip silently. Keep the semaphore and jitter.
  - Watcher: on `AlreadySyncing`, mark the Source dirty and run exactly one follow-up Sync once the claim is free.
- `src/cli/mod.rs`, `src/mcp/mod.rs`: surface "already syncing" as a clear message (MCP: `isError: false`, the outcome in the payload).
- `AGENTS.md` Lessons: replace the `status = 'syncing'` scheduler rule with the claim rule.

## Red-First Test Cases
1. `sources::claim::tests::second_claim_is_refused`
2. `sources::claim::tests::dead_holder_claim_is_taken_over` (fake liveness hook)
3. `sources::claim::tests::expired_claim_is_taken_over` (fake clock)
4. `sources::claim::tests::same_pid_other_token_is_taken_over`: a claim left by an earlier run with our pid but a different token counts as dead.
5. `sources::tests::concurrent_sync_returns_already_syncing`: two `sync` futures on one Source; exactly one `Synced`.
6. `sources::tests::remove_while_syncing_returns_already_syncing`
7. `ui::api::tests::sync_returns_409_when_claimed`
8. `daemon::tests::scheduler_picks_source_with_dead_claim`: a crashed sync no longer blocks refreshes.
9. `daemon::tests::watcher_runs_follow_up_after_already_syncing`

## Acceptance Criteria
- No in-memory exclusion of Sources remains in `daemon/`.
- A claim row never outlives a finished Sync (success or error).

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
