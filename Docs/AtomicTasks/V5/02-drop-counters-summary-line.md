# Task V5-02: Count dropped pages and log a `web_sync_summary` line

- **Phase:** V5
- **Dependencies:** none
- **Suggested Commit:** `feat(ingest): count dropped pages and log a web_sync_summary line`

## Goal
Every web Sync with a valid Origin logs one `web_sync_summary` line that counts its results and the pages `build_page_outcomes` dropped, by reason. Done when `dropped_pages_are_counted_by_reason` passes.

## Exact Files
- `src/ingest/web.rs`:
  - **New struct** `#[derive(Debug, Default)] pub(crate) struct WebSyncAccounting` with only the fields this task reads:
    - `results_stored`, `results_failed`
    - `unparsable_url`, `redirected_out_of_scope`, `url_not_storable`, `duplicate_result`, `binary_file` (all `usize`)
    - `cut_off: bool`

    The other fields come in tasks 03, 05 and 06, each together with the code that reads it. No `allow(dead_code)` is used. The final form is at design lines 395-421.
  - **`sync_web_source_with_accounting(db_path: &Path, source: &SourceRecord, prune: bool) -> (Result<SyncSummary>, WebSyncAccounting)`** (crate-private):
    - It creates `WebSyncAccounting::default()` and calls an inner `async fn`. That function holds today's body of `sync_web_source` with `?` intact, takes `&mut WebSyncAccounting` and returns `Result<SyncSummary>`.
    - If the Origin fails today's checks (URL parse or `WebScope::new`), it returns the default record and logs no line.
    - Otherwise it logs the summary line at `info` just before returning, on both the success and the error path.
    - It sets `cut_off` with today's rule, `outcomes.len() >= max_pages`, so behaviour does not change.
    - `sync_web_source` calls it and returns the `Result`.
  - **`execute_web_crawl(source, origin, scope, acc: &mut WebSyncAccounting)`:** same return type as today; it passes `acc` on.
  - **`build_page_outcomes(pages, scope, acc: &mut WebSyncAccounting)`:** same return type as today.
    - It counts the drops at lines 670 (`unparsable_url`), 676 (`redirected_out_of_scope`), 682 (`url_not_storable`), 687 (`duplicate_result`) and 707 (`binary_file`).
    - It sets `results_stored` and `results_failed`.
    - The `continue` at line 703 is a failed result, not a drop.
    - A page that fits two reasons is counted under the first it meets.
  - **Log line** (one line; text values quoted):
    `web_sync_summary source="<name>" origin="<url>" ok=<bool> max_pages=<n> cut_off=<bool> results_stored=<n> results_failed=<n> unparsable_url=<n> redirected_out_of_scope=<n> url_not_storable=<n> duplicate_result=<n> binary_file=<n>`.
    Task 03 adds the remaining fields in the order given at design lines 205-209.
- `Docs/SPEC.md` §4.1: move the summary-line bullet out of "Planned: Sync accounting" and into current text, listing the fields this task logs.

## Red-First Test Cases
1. `ingest::web::tests::dropped_pages_are_counted_by_reason`:
   - It calls `build_page_outcomes` with hand-made pages. spider's `Page` derives `Default`, and `set_url`, `status_code`, `final_redirect_destination` and `binary_file` are public.
   - Pages: one per reason (5), one failed page (status 404) and one stored page.
   - It asserts that each drop counter is 1, and that `results_stored` and `results_failed` are 1.

## Acceptance Criteria
- Every existing test in `src/ingest/web.rs` still passes without changes, in particular `honors_max_pages_and_include_override`, `recrawl_records_diff_marks_removed_stale_and_500_error` and `seeds_from_sitemap_respects_robots_and_filters_patterns`.
- `SyncSummary` and `src/ingest/local.rs` are unchanged.
- `stdout` stays free of logs (`citadel mcp` purity).

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
