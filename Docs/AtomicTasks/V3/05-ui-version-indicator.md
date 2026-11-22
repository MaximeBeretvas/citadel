# Task V3-05: Web UI `/api/version` endpoint & subtle update badge

- **Phase:** V3
- **Dependencies:** V3-03
- **Suggested Commit:** `feat(ui): expose GET /api/version and show update badge in Web UI`

## Goal
Expose `GET /api/version` in the embedded Web UI server backed by `update::check` and render a subtle, non-intrusive update badge in the UI header whenever a newer version of Citadel is available.

## Exact Files
- `src/ui/api.rs`:
  - Add `GET /api/version` handler returning JSON `{ current_version, latest_version, update_available, release_url }` read from `update-check.json` (triggering a non-blocking background refresh when stale unless `config.no_update_check` is true).
- `src/ui/mod.rs`:
  - Mount `.route("/api/version", get(api::get_version))` on the Axum router.
- `src/ui/assets/index.html`, `src/ui/assets/app.js`, `src/ui/assets/styles.css`:
  - Fetch `/api/version` on page load (failing silently if unavailable).
  - When `update_available === true`, reveal a subtle header badge/pill (e.g., `Update vX.Y.Z available`) with a tooltip or popover showing `citadel update` and a link to `release_url`.
  - Zero external CDN requests (100% offline-compatible).

## Red-First Test Cases
1. `ui::tests::get_version_returns_current_version_when_cache_missing`: `GET /api/version` returns HTTP 200 with `current_version == env!("CARGO_PKG_VERSION")` and `update_available == false`.
2. `ui::tests::get_version_reports_update_available_when_cache_has_newer_semver`: seed tempdir `update-check.json` with `v99.0.0`, call `GET /api/version`, verify `update_available == true`, `latest_version == "99.0.0"`, and `release_url` present.
3. `ui::tests::get_version_reports_up_to_date_when_cache_matches_current`.

## Acceptance Criteria
- `GET /api/version` responds immediately from cache without blocking on network I/O.
- UI header stays clean/unchanged when `update_available == false`.
- Embedded UI assets compile cleanly via `rust-embed` with zero external requests.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
