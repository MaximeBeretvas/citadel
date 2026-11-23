# Task V4-01: `render_js` waits for JS-built links on a per-crawl browser

- **Phase:** V4
- **Dependencies:** none
- **Suggested Commit:** `feat(ingest): render JS-built links with a per-crawl headless browser`

## Goal
A `render_js` web Source discovers links that scripts add after load, concurrent `render_js` Syncs
do not collide, and a missing browser fails the Sync with an actionable error.

## Exact Files
- `src/ingest/browser.rs` (new, `chrome` only): `RenderBrowser::launch` starts headless Chrome
  with a private `tempfile` profile; `websocket_url`; `shutdown`; `NO_BROWSER_ERROR`.
- `src/ingest/web.rs`:
  - `configure_website(.., render_js)`: when true, `with_wait_for_idle_network0` +
    `with_wait_for_idle_dom("body")`, each capped by `RENDER_WAIT_TIMEOUT` (10 s).
  - `run_website_crawl`: `prepare_render_browser` (own browser, or `CHROME_URL` checked via
    `ensure_render_browser`), crawl, then `shutdown`.
  - User-Agent `citadel/<CARGO_PKG_VERSION>` instead of the hard-coded `citadel/0.1`.
- `Cargo.toml`: `tempfile` becomes an optional dependency enabled by `chrome`.

## Red-First Test Cases
1. `ingest::web::tests::static_crawl_cannot_see_script_built_links`: `render_js: false` stores 1 page.
2. `ingest::web::tests::render_js_crawl_follows_script_built_links` (`chrome`): index + 2
   script-built pages = 3.
3. `ingest::web::tests::concurrent_render_js_syncs_each_get_their_own_browser` (`chrome`): two
   Syncs joined concurrently each store 3 pages.
4. `ingest::web::tests::render_js_without_reachable_browser_is_an_actionable_error` (`chrome`):
   unreachable browser → error naming Chrome and `CHROME_URL`.

## Acceptance Criteria
- No Chrome process or `citadel-chrome-*` profile directory remains after a Sync.
- Non-`render_js` Sources never start a browser.
- Lite build (`--no-default-features`) compiles and passes its tests.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
```
