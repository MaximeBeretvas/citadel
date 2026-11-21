# Task 10: Embedded Offline Web UI Assets (`rust-embed`)

- **Phase:** 2 (Full V1)
- **Dependencies:** [09-ui-rest-api-and-server.md](./09-ui-rest-api-and-server.md)
- **Suggested Commit:** `feat(ui): embed lightweight offline Web UI assets via rust-embed`

## Goal
Build and embed the lightweight, zero-CDN static Web UI (`src/ui/assets/`) using `rust-embed` + `mime_guess` per [Docs/SPEC.md](../../SPEC.md#L425-L430), covering the 4 focused views: **Sources**, **Knowledge Browser**, **Changes & Health**, and **SQL Explorer**.

## Exact Files
- `src/ui/assets/index.html`
- `src/ui/assets/style.css`
- `src/ui/assets/app.js`
- `src/ui/mod.rs` (serve embedded assets + `/` fallback)

## Red-First Test Cases (Write Before Implementation)
1. `ui::tests::serves_embedded_index_css_and_js_with_correct_mime`: `GET /`, `GET /style.css`, and `GET /app.js` return `200 OK` with `text/html`, `text/css`, and `application/javascript` headers.
2. `ui::tests::assets_contain_zero_external_cdn_links`: unit test asserts embedded HTML/CSS/JS contain no `http://` or `https://` `<script>`/`<link>` external CDN references.
3. `ui::tests::unknown_asset_returns_404`: `GET /nonexistent.js` returns `404 Not Found`.

## Acceptance Criteria
- Clean, fast, responsive UI with dark/light support and zero build step (vanilla HTML/CSS/JS).
- **Sources view:** table of web + local sources, status badges, page/stale/error counts, Add Source modal (URL or folder), Sync Now & Delete buttons.
- **Knowledge Browser:** filter by source or search text, view page Markdown and heading sections.
- **Changes & Health:** recent diffs (`v_recent_changes`) with colored `+`/`-` unified diff viewer + list of `stale`/`error` pages.
- **SQL Explorer:** interactive read-only SQL box calling `POST /api/query` with tabular result rendering and quick example presets.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
