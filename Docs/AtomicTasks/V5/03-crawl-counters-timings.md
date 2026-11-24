# Task V5-03: Time each web Sync step and count later crawls

- **Phase:** V5
- **Dependencies:** V5-02
- **Suggested Commit:** `feat(ingest): time each web Sync step and count later crawls`

## Goal
The `web_sync_summary` line also gives the time per step, the pages received and lost, the drops made by later crawls, the later crawls started and the delay in force. With these numbers, pages received equals results plus the six drop counts. Done when `accounting_times_every_step_and_counts_later_crawls` passes.

## Exact Files
- `src/ingest/web.rs`:
  - **Fields added to `WebSyncAccounting`:**
    - `delay_ms: Option<u64>`
    - `pages_received`, `lost_by_listener`, `already_collected`, `later_crawls_started` (`usize`)
    - `discovery_ms`, `first_crawl_ms`, `later_crawls_ms`, `conversion_ms`, `write_ms` (`Option<u64>`)
  - **`run_website_crawl(website: &mut Website, render_js: bool) -> Result<(Vec<Page>, usize)>`:**
    - The spawned collector (lines 540-555) returns its pages together with the sum of its `skipped` values (line 548), converted to `usize`.
    - The stall path (lines 575-583) returns an error, as today. It only occurs with `render_js`.
  - **`execute_web_crawl`:**
    - It times discovery, the first crawl, the later crawls and the `build_page_outcomes` call (`conversion_ms`).
    - It sets `later_crawls_started` and `delay_ms`; `delay_ms` comes from `Website::get_delay()` after the first crawl.
    - It counts the later-crawl drops at line 380 (`unparsable_url`) and line 383 (`already_collected`).
    - It adds each crawl's pages and lost count to `pages_received` and `lost_by_listener`.
  - **`sync_web_source_with_accounting`:** times `persist_sync_outcomes` into `write_ms`.
  - **Log line:** gains the new fields in the order of design lines 205-209. A `None` value prints as `-`.
- `Docs/SPEC.md` §4.1: the current summary-line text lists all fields. Remove the "Planned" marker from that bullet.

## Red-First Test Cases
1. `ingest::web::tests::accounting_times_every_step_and_counts_later_crawls`:
   - **Setup:** the default `max_pages`. A served Origin links to pages `a` and `b`. An `llms.txt` lists two hidden pages, `h1` and `h2`, under the Origin's path. The hidden pages are not linked from the Origin, do not link to each other, and each links back to the Origin. There is no `sitemap.xml`.
   - **How it runs:** `SourceLifecycle::add(params, false)`, then `sync_web_source_with_accounting(&cfg.db_path(), &added.source, false).await`.
   - **Assertions:**
     - `later_crawls_started == 2`
     - `already_collected == 6`
     - `pages_received == 11`
     - every duration is `Some`
     - `pages_received == results_stored + results_failed +` the six drop counts

## Acceptance Criteria
- All existing tests still pass without changes.
- Run a foreground daemon on a scratch data directory and capture its stderr. The `web_sync_summary` line of the captured Sync shows every field, and task 01's script reads it.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
