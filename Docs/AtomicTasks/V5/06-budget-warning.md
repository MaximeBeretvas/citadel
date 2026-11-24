# Task V5-06: Warn when a cut-off Sync spent its budget on redirected pages

- **Phase:** V5
- **Dependencies:** V5-05
- **Suggested Commit:** `feat(ingest): warn when a cut-off Sync spent its budget on redirected pages`

## Goal
A cut-off Sync that lost much of its page budget to pages redirecting outside the Source adds a warning to its Sync summary, which the CLI, UI and MCP already show. The warning names up to three example URLs. Done when the 5 `budget_warning` unit tests pass.

## Exact Files
- `src/ingest/web.rs`:
  - **Field added to `WebSyncAccounting`** (not printed): `redirect_examples: Vec<String>`. `build_page_outcomes` pushes the requested URL of the first three pages it drops as `redirected_out_of_scope`.
  - **Constants:** `MIN_REDIRECTS_FOR_WARNING: usize = 5` and `REDIRECT_SHARE_PERCENT: usize = 10`.
  - **New pure function** `budget_warning(accounting: &WebSyncAccounting, max_pages: usize) -> Option<String>`. It returns the sentence when all three of these hold:
    - `cut_off`
    - `redirected_out_of_scope >= MIN_REDIRECTS_FOR_WARNING`
    - `redirected_out_of_scope * 100 > pages_received * REDIRECT_SHARE_PERCENT`

    The sentence: "`<redirected>` of `<received>` pages received redirected outside this Source and were not stored, for example `<up to three URLs>`. They count against `max_pages` (`<max_pages>`), so other pages were not reached. Raise `max_pages`, or exclude that section if you do not need it."
  - **`sync_web_source_with_accounting`:** passes `source.max_pages as usize` and pushes the sentence into the Sync's warnings before `persist_sync_outcomes`.
- `Docs/SPEC.md` §4.1: the warning bullet becomes current text and loses its "Planned" marker.

## Red-First Test Cases
Unit tests on `budget_warning`:
1. `warns_when_budget_went_to_redirects`: 5 of 20 received, cut off. Gives one warning naming the 3 examples.
2. `no_warning_below_five_redirects`: 4 of 20, cut off.
3. `no_warning_at_ten_percent`: 5 of 50, cut off.
4. `warns_above_ten_percent`: 5 of 49, cut off.
5. `no_warning_when_not_cut_off`: 5 of 20, not cut off.

## Acceptance Criteria
- No existing Sync warning changes.
- **Q2 (open):** the author may change the two thresholds after task 08. Only the constants and tests 2 to 4 then change.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
