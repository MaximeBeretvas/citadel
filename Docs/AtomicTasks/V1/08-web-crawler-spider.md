# Task 08: Web Crawler & Diff Ingestion (`spider` + `spider_transformations`)

- **Phase:** 2 (Full V1)
- **Dependencies:** [05-local-markdown-ingestion.md](./05-local-markdown-ingestion.md)
- **Suggested Commit:** `feat(ingest): implement web crawling and diff sync with spider`

## Goal
Implement `src/ingest/web.rs` using **`spider` + `spider_transformations`** (MIT) per [Docs/SPEC.md](../../SPEC.md#L229-L238): sub-URL discovery within path prefix, `sitemap.xml` & `llms.txt` seeding, readability HTML-to-Markdown conversion, politeness/rate limits, optional `chrome` feature flag for `render_js`, and `stale`/`error` status transitions on re-crawl.

## Exact Files
- `src/ingest/web.rs`
- `src/ingest/mod.rs` (dispatch `kind = 'web'` to `web::sync_web_source`)
- `Cargo.toml` (ensure `spider` + `spider_transformations` + optional `chrome` feature)

## Red-First Test Cases (Write Before Implementation)
1. `web::tests::crawls_local_fixture_site_within_prefix_and_extracts_markdown`: spins up a local Axum test server with `/docs/`, `/docs/guide`, `/docs/api`, and `/blog/off-topic`; asserts only `/docs/*` pages are stored with clean Markdown and H1–H3 sections.
2. `web::tests::seeds_urls_from_llms_txt_when_present`: fixture serves `/llms.txt` linking an unlinked `/docs/hidden`; asserts `/docs/hidden` is discovered and stored.
3. `web::tests::recrawl_records_diff_marks_removed_stale_and_500_error`: second crawl modifies `/docs/guide`, removes `/docs/api`, and returns `500` on `/docs/broken`; asserts `page_revisions` recorded for `/docs/guide`, `/docs/api` marked `stale`, `/docs/broken` marked `error` with previous `content_md` preserved.
4. `web::tests::render_js_without_chrome_feature_returns_clear_warning`: when built without `--features chrome`, `render_js = true` logs/returns an actionable message instead of failing cryptically.

## Acceptance Criteria
- Respects `max_pages`, `max_depth`, `include_patterns`, `exclude_patterns`, `respect_robots_txt(true)`, and bounded concurrency (default 4, 100 ms delay).
- Never silently deletes pages that disappear or fail on re-crawl (`stale` / `error` status).
- Works with both `citadel add <url>` CLI and MCP `manage(action="add", origin="<url>")` (keeping SSRF guard active for MCP, with test hook for local fixture server).

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
