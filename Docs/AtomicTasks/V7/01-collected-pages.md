# Task V7-01: Move collection, classification, cut-off and counting into `CollectedPages`

- **Phase:** V7
- **Dependencies:** V6-01
- **Suggested Commit:** `refactor(ingest): decide every collected page in one module`

## Goal
One in-process module, `CollectedPages`, takes every Collected page of a web Sync. It decides each page's fate, the cut-off, the missing-page policy and the budget warning, and it owns every page count on the `web_sync_summary` line. Behaviour is identical.

Done when:
- the new interface tests in `ingest::web::collected::tests` pass;
- the old helper tests are deleted;
- every fixture-server web test passes with unchanged assertions.

## Interface
```text
CollectedPages::new(scope, max_pages)
  add_first_crawl(pages, lost)         keeps every page; unparsable URLs and duplicates are judged in finish
  next_later_crawl(seed) -> Skip | Stop | Crawl(budget)
                                       Skip: seed already collected; Stop: budget spent;
                                       Crawl: remaining document budget (an X/X.md pair counts once)
  add_later_crawl(seed, pages, lost)   de-duplicates at intake (unparsable_url, already_collected), marks the seed visited
  counts() -> &PageCounts              the counts so far, for the log line of a Sync whose crawl failed
  finish(prune) -> (Result<Collected>, PageCounts)
                                       Page outcomes, missing-page policy and warnings, or the "nothing reachable"
                                       error; the page counts (incl. cut_off) come back either way
```

`WebSyncAccounting` becomes `{ pages: PageCounts, delay_ms, <five step durations> }`. The owner chose this nesting on 2026-11-24. As a result, the two fixture tests that read counters change only their field paths (`acc.x` becomes `acc.pages.x`). Their asserted values stay the same.

## Exact Files
- `src/ingest/web/collected.rs` (new) holds `CollectedPages` and its result.
  - **Moved in:** `scoped_urls`, `count_scoped_urls`, `count_scoped_documents`, `build_page_outcomes`, `pair_documents`, `Kept`, `is_cut_off`, `result_count`, `budget_warning` and its thresholds, the missing-page policy choice, the "nothing reachable" check, and the intake de-duplication from `run_later_crawls`.
  - **Becomes internal:** the index map between `pair_documents` and `build_page_outcomes`.
- `src/ingest/web.rs`:
  - `execute_web_crawl` and `run_later_crawls` drive `CollectedPages`.
  - `sync_checked_web_source` takes the policy and warnings from `finish`.
  - `WebSyncAccounting` is assembled from the collector's page counts plus the caller's `delay_ms` and step durations. The "Not printed" carrier fields (`first_crawl_received`, `first_crawl_lost`, `collected_storable_urls`, `redirect_examples`) leave it.
  - `confirm_render_failures` runs per rendered crawl, inside `run_website_crawl`'s render path, before the pages are handed over.
  - The `web_sync_summary` line is byte-identical in field order and meaning.
- `Docs/SPEC.md` §4.1 and §4: function names point at `CollectedPages`, and SPEC:280 says Retired copies count toward the results condition.
- `CONTEXT.md`: Collected page (new) and Cut-off Sync (sharpened). Both were already written during the design session.

## Red-First Test Cases (`ingest::web::collected::tests`)
Write these against the interface first. They fail to compile until `CollectedPages` exists.

1. **Drop reasons.** `counts_each_dropped_page_under_its_first_reason` replaces `dropped_pages_are_counted_by_reason`, the same scenario run through intake and `finish`. Also `later_crawl_drops_duplicates_and_unparsable_urls_at_intake`.
2. **Conservation.** `every_received_page_lands_in_exactly_one_count`: `pages_received` = `results_stored` + `results_failed` + the eight drop counts, across a first crawl and a later crawl.
3. **Pairing.** These replace the unit tests of the same scenarios:
   - `twins_and_canonical_duplicates_are_counted_and_retired`
   - `a_failed_first_copy_of_a_page_claims_no_twin`
   - `canonical_names_the_exact_trailing_slash_variant`
   - `canonical_to_another_path_does_not_collapse_pages`
4. **Later-crawl budget.** `later_crawl_budget_counts_a_markdown_twin_pair_once`, plus `next_later_crawl_skips_a_collected_seed_and_stops_when_the_budget_is_spent`.
5. **Cut-off.** These replace the four `is_cut_off` truth-table tests:
   - `first_crawl_at_limit_is_cut_off`
   - `collected_urls_at_limit_is_cut_off`
   - `results_at_limit_count_retired_copies`
   - `under_budget_is_complete`
6. **Missing-page policy.** `policy_is_prune_with_prune_preserve_when_cut_off_else_mark_stale`.
7. **Budget warning.** These replace the five `budget_warning` tests, driven by hand-made redirected pages:
   - `warns_when_budget_went_to_redirects`
   - `no_warning_below_five_redirects`
   - `no_warning_at_ten_percent`
   - `warns_above_ten_percent`
   - `no_warning_when_not_cut_off`
8. **Nothing reachable.** `nothing_reachable_fails_with_the_first_error`, plus `an_http_error_page_alone_is_still_a_result`.

**Kept unchanged:**
- `reads_link_tags_from_minified_html` (internal HTML parsing);
- `failed_step_keeps_its_duration` (the clock stays with the caller);
- every fixture-server and render test, in particular:
  - `accounting_times_every_step_and_counts_later_crawls`, whose conservation assertion proves the wiring;
  - `cut_off_sync_with_dropped_page_marks_nothing_stale`, which proves persistence applies Preserve;
  - `honors_max_pages_and_include_override`;
  - the `markdown_twins` integration tests.

**Deleted:** the old in-process tests that called `build_page_outcomes`, `count_scoped_*`, `result_count`, `is_cut_off` and `budget_warning` directly.

## Acceptance Criteria
- Every fixture-server web test passes with unchanged assertions. The only edits are the `acc.pages.x` field paths.
- No function outside `collected.rs` reads a field of a collected page to decide its fate.
- `--no-default-features` (lite) compiles and passes.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
