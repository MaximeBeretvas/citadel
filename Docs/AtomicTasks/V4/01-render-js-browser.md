# Task V4-01: `render_js` waits for JS-built links on a per-crawl browser

- **Phase:** V4
- **Dependencies:** none
- **Suggested Commit:** `feat(ingest): render JS-built links with a per-crawl headless browser`

## Goal
A `render_js` web Source discovers links that scripts add after load, concurrent `render_js` Syncs
do not collide, and a missing browser fails the Sync with an actionable error.

## Exact Files
- `src/ingest/browser.rs` (new, `chrome` only): `RenderBrowser::launch` starts headless Chrome
  with a private `tempfile` profile; `websocket_url`; `shutdown`; `NO_BROWSER_ERROR` (plus the
  first line of the cause). Executable: `CHROME` env, else the first of `chromium`,
  `chromium-browser`, `google-chrome-stable`, `google-chrome`, `chrome` on `PATH` (spider builds
  chromey without `auto-detect-executable`, so Debian's `/usr/bin/chromium` is otherwise never
  found), else chromey's fixed paths. Chrome's sandbox stays on natively and is disabled only
  inside a container (`update::is_running_in_container`), where it cannot start as non-root.
- `src/ingest/web.rs`:
  - `configure_website(.., render_js)`: when true, `with_wait_for_idle_network0` +
    `with_wait_for_idle_dom("body")`, each capped by `RENDER_WAIT_TIMEOUT` (10 s).
  - `run_website_crawl`: `prepare_render_browser` (own browser, or `CHROME_URL` checked via
    `ensure_render_browser`), crawl, then `shutdown`.
  - User-Agent `citadel/<CARGO_PKG_VERSION>` instead of the hard-coded `citadel/0.1`.
- `Cargo.toml`: `tempfile` becomes an optional dependency enabled by `chrome`.

### Reliability (found by looping the concurrent test)
- **No Sync may hang:** the browser event loop keeps running through ordinary protocol errors
  (ending it left the final close request unanswered forever); each shutdown step is capped at
  5 s; `CrawlActivity::stalled_for` stops a `render_js` crawl when no page finishes for
  `RENDER_STALL_LIMIT` (5 min) and fails the Sync with previous pages kept.
- **spider's sitemap pass is off for `render_js`** (`with_ignore_sitemap(true)`): Citadel seeds
  from sitemaps itself (`discover_seed_urls`), and spider's pass reconnects to the browser after
  the crawl.
- **Browser-reported 4xx is confirmed with the server** (`confirm_render_failures`): about 1 run
  in 50, spider reports a page with the right URL and full rendered content but status 404 (the
  only 404 the browser received was its own `/favicon.ico` request). A client error is kept only
  when a direct request agrees.

### Content of rendered pages (found on the real Meraki docs)
`PREPARE_RENDERED_PAGE_JS` runs on every rendered page (spider `with_execution_scripts`, path
`/`) after the page settles and before its HTML is read:
- **Shadow DOM:** DevNet draws each endpoint's documentation inside a web component's shadow
  root, which a page's HTML does not include, so 829 of 841 synced pages held only the sidebar.
  Every open shadow root is copied, innermost first, into a plain element under its host.
- **Navigation:** the script-built sidebar was 343 KB on every page. A list or nav block with
  30+ links and over 80% link text is removed when at least 200 characters of other text remain,
  so an index page keeps its list. Links are still discovered (checked on the real site).

## Red-First Test Cases
1. `ingest::web::tests::static_crawl_cannot_see_script_built_links`: `render_js: false` stores 1 page.
2. `ingest::web::tests::render_js_crawl_follows_script_built_links` (`chrome`): index + 2
   script-built pages = 3.
3. `ingest::web::tests::concurrent_render_js_syncs_each_get_their_own_browser` (`chrome`): two
   Syncs joined concurrently each store 3 pages.
4. `ingest::web::tests::render_js_without_reachable_browser_is_an_actionable_error` (`chrome`):
   unreachable browser → error naming Chrome and `CHROME_URL`.
5. `ingest::browser::tests::finds_debian_chromium_on_path` /
   `ignores_non_executable_files_and_missing_path`.
6. `ingest::browser::tests::chrome_sandbox_is_kept_natively_and_dropped_only_in_containers`.
7. `ingest::browser::tests::launch_error_keeps_the_fix_hint_and_the_cause`.

8. `ingest::web::tests::stall_watchdog_fires_only_after_pages_stop_arriving`.
9. `ingest::web::tests::browser_client_error_is_kept_only_when_the_server_agrees` /
   `misreported_page_error_is_corrected_from_the_server`.

10. `ingest::web::tests::render_js_stores_content_drawn_inside_shadow_dom`.
11. `ingest::web::tests::render_js_drops_a_big_navigation_list_next_to_real_content` /
    `render_js_keeps_a_link_list_that_is_the_page_content`.

Chrome-launching tests share `CHROME_TEST_LOCK` so browsers start one test at a time and do not
starve timing-sensitive tests (e.g. `dropped_sync_future_still_finishes_in_background`).

## Acceptance Criteria
- No Chrome process or `citadel-chrome-*` profile directory remains after a Sync.
- Non-`render_js` Sources never start a browser.
- Inside the default Docker image, `citadel add <meraki api-v1> --render-js --max-pages 25` stores
  25 pages (was 2).
- Real site, 6 pages (`--max-pages 6`, under 2 minutes): each page has its own content of
  1–10 KB (was the same 343 KB sidebar), e.g. `get-organizations` holds the operation id,
  description, parameters and response schema.
- 60 consecutive runs of `concurrent_render_js_syncs_each_get_their_own_browser`: no hang, no
  failure.
- Lite build (`--no-default-features`) compiles and passes its tests.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
```
