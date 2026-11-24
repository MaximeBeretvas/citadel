# Task V5-05: Keep pages of a cut-off Sync even when fetched pages were dropped

- **Phase:** V5
- **Dependencies:** V5-03
- **Suggested Commit:** `fix(ingest): keep pages of a cut-off Sync even when fetched pages were dropped`

## Goal
A Sync that ran out of its page budget marks no page stale, even when some fetched pages got no result. This fixes the `dlthub` defect of 2026-11-23, where 197 pages were marked stale. Done when `cut_off_sync_with_dropped_page_marks_nothing_stale` passes; it fails on today's code.

## Exact Files
- `src/ingest/web.rs`:
  - **Fields added to `WebSyncAccounting`** (not printed on the log line): `first_crawl_received`, `first_crawl_lost`, `collected_storable_urls` (`usize`).
  - **`execute_web_crawl`:**
    - After the first crawl, it sets `first_crawl_received` and `first_crawl_lost`.
    - After the later-crawl loop, it sets `collected_storable_urls = count_scoped_pages(&collected_pages, scope)`. That helper is at lines 646-657, and the loop already uses it.
  - **New pure function** `is_cut_off(first_crawl_delivered_or_lost: usize, collected_storable_urls: usize, outcomes: usize, max_pages: usize) -> bool`. It is true when any one of three conditions holds:
    1. `first_crawl_delivered_or_lost >= max_pages`
    2. `collected_storable_urls >= max_pages`
    3. `outcomes >= max_pages`
  - **`sync_web_source_with_accounting`:**
    - It calls `is_cut_off(acc.first_crawl_received + acc.first_crawl_lost, acc.collected_storable_urls, outcomes.len(), source.max_pages as usize)`.
    - It stores the result in `acc.cut_off`.
    - It chooses `MissingPagePolicy::Preserve` from it, instead of the comparison at line 308. `--prune` still wins.
- `CONTEXT.md`:
  - The "Stale page" entry's second sentence becomes: "A cut-off Sync leaves unvisited Pages as they were."
  - New entry **Cut-off Sync**: "A Sync whose first crawl used up the page budget (`max_pages`), or whose distinct storable pages collected, or pages recorded, reach it, whether or not those pages were stored."
- `Docs/SPEC.md` §4.1:
  - Replace "Today's cut-off rule" and the "Known defect" bullet with the current three-condition rule.
  - Remove that bullet from "Planned".
- `Docs/PRD.md` FR-2.4: remove *(planned; see FR-2.7)*.

## Red-First Test Cases
1. `ingest::web::tests::cut_off_sync_with_dropped_page_marks_nothing_stale` (served site):
   - **Setup:** Origin `http://{addr}/docs/` with `include_patterns: ["docs/p*"]` and `max_pages` 3, linking to `/docs/p1` to `/docs/p5`.
   - **Why the Origin is dropped:** it is crawlable (the exception at `web.rs:188`) but not storable (`web.rs:198`), so it is dropped as `url_not_storable`. The first crawl delivers 3 pages, and the Sync has 2 results.
   - **Steps:**
     1. `SourceLifecycle::add(params, false)`.
     2. Insert an `active` row for `/docs/archived`, which is never served. Do it as at `web.rs:2405-2412`.
     3. `sync_web_source_with_accounting`.
   - **Today:** `pages_stale == 1`.
   - **After this task:** `cut_off == true`, `pages_stale == 0`, and the row is still `active`.
2. Unit tests on `is_cut_off`:
   - `first_crawl_at_limit_is_cut_off`: `(1000, 759, 756, 1000) == true` (the `dlthub` numbers; condition 1 only).
   - `collected_urls_at_limit_is_cut_off`: `(40, 100, 90, 100) == true`.
   - `todays_condition_still_counts`: `(2, 2, 3, 3) == true`.
   - `under_budget_is_complete`: `(40, 60, 60, 100) == false`.

## Acceptance Criteria
- `honors_max_pages_and_include_override` and `recrawl_records_diff_marks_removed_stale_and_500_error` pass unchanged.
- A Sync under its budget still marks removed pages stale.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
