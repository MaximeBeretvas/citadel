# Task V6-01: Store one Page per document for Markdown twins and canonical duplicates

- **Phase:** V6
- **Dependencies:** V5-05
- **Suggested Commit:** `fix(ingest): store one page per document for markdown twins`

## Goal
A web Sync stores a page and its Markdown twin (`X`, `X.md`) as one Page at `X`, and a canonical duplicate (`X?version=2`) only as `X`. Done when `stores_one_page_per_markdown_twin` passes; it fails on the V5 code.

## Exact Files
- `src/ingest/web.rs`:
  - `pair_documents` + `head_links` (`lol_html`), `markdown_twin_keys`.
  - `build_page_outcomes` emits one `Ready` per document and `Retired` per merged copy; counters `markdown_twin`, `canonical_duplicate` on `WebSyncAccounting` and the `web_sync_summary` line.
  - `count_scoped_documents` sizes later crawls; `is_cut_off` keeps counting every URL (`count_scoped_urls`) and every result; `result_count` (no `Retired`) feeds the "nothing reachable" check.
- `src/ingest/local.rs`: `PageIngestOutcome::Retired { uri }`, `retire_visited_pages`, and `retire_page` shared with `reconcile_missing_pages`.
- `Cargo.toml`: direct `lol_html = "2"` (already in the tree through spider, also in the lite build).
- `Docs/SPEC.md` §4.1, `CONTEXT.md` (Markdown twin, Canonical duplicate, Stale page).

## Red-First Test Cases (`ingest::web::tests::markdown_twins`)
1. `stores_one_page_per_markdown_twin`: served site with `rel=alternate` twins, a suffix-only twin in minified HTML, an `llms.txt` listing the `.md` URLs, an orphan `.md`, and a `?version=2` duplicate. One active Page per document, with `.md` content and frontmatter titles.
2. `resync_marks_stored_markdown_twins_stale`: old twin rows go stale on a complete Sync and on a cut-off Sync (an unvisited row stays active there); `--prune` deletes them.
3. `markdown_only_site_is_crawled_normally`, `failed_twin_falls_back_to_the_other_copy`.
4. Unit: `reads_link_tags_from_minified_html`, `canonical_to_another_path_does_not_collapse_pages`, `twins_and_canonical_duplicates_are_counted_and_retired`, `later_crawl_budget_counts_a_markdown_twin_pair_once`, `a_failed_first_copy_of_a_page_claims_no_twin`, `canonical_names_the_exact_trailing_slash_variant`.

## Acceptance Criteria
- Every existing web test passes unchanged.
- Re-syncing real Sources leaves no active `uri` / `uri || '.md'` pairs (see the PR for the before/after table).

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo test sources
```
