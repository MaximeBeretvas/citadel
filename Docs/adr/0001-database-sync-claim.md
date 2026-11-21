# Sync claims live in the database, not in process memory

The CLI, `web-vault mcp` (stdio) and `serve`/`daemon` run as separate processes against one vault, so an in-memory lock (the scheduler's old in-flight set) cannot stop two of them syncing the same Source at once. A Sync claim is therefore a row in a dedicated `sync_claims` table (primary key `source_id`, holder `pid`, `claimed_at`): inserting the row takes the claim, deleting it releases it, and a claim whose holder pid is no longer alive, or that is older than a hard age limit, may be taken over.

## Considered Options

- **In-process mutex / in-flight set**: rejected; invisible to other processes.
- **Expiring lease with heartbeat**: rejected; pages persist only at the end of a crawl, so a heartbeat would need its own background task.
- **Claim columns on `sources`, or a pid on the running `sync_runs` row**: rejected; both need `ALTER TABLE`, and the schema has no migration mechanism (`init_db` re-applies `CREATE … IF NOT EXISTS`). A new table reaches existing vaults without one.

## Consequences

- Pid liveness is only meaningful on one host. A vault volume shared between a container and the host relies on the age limit alone.
- `sources.status = 'syncing'` remains the displayed status; `sync_claims` is the source of truth for exclusion.
