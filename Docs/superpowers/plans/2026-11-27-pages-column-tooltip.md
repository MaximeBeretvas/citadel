# Sources `Pages` Column Tooltip & Drill-Down Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Sources table `Pages` column self-explanatory via an accessible top-layer tooltip (hover, focus, screen reader) and let users click non-zero active/stale/error counts to drill down into `#browser` with `http_status` and `error_message` displayed.

**Architecture:** Frontend-only update across the `rust-embed` assets (`index.html`, `style.css`, `app.js`). Each row renders upfront `.sr-only` breakdown text wired via `aria-describedby` for browse-mode screen readers, paired with a single shared top-layer `<div id="pages-tooltip" popover="hint" role="tooltip">` positioned via `getBoundingClientRect()` (avoiding `.panel`/`.table-wrap` overflow clipping with zero external assets or polyfills). Non-zero counts render as native `<button>` controls that navigate to `#browser?source=<id>&status=<status>` and surface `http_status` + `error_message` in `renderPageDetail()`.

**Tech Stack:** Vanilla HTML5 (`popover`), CSS3 (custom properties, light/dark themes, `prefers-reduced-motion`), ES2020 JS (`app.js`), Rust Axum/rust-embed tests (`src/ui/mod.rs`), headless Chrome visual verification (`REVIEW.md`).

**Spec:** `Docs/superpowers/specs/2026-11-27-pages-column-tooltip-design.md`

## Global Constraints

- Zero external assets, CDNs, or npm polyfills (`assets_contain_zero_external_cdn_links` test must stay green).
- Cross-browser without polyfills (Chrome, Firefox, Safari): feature-detect `popover="hint"` and fall back to `popover="manual"`.
- WCAG 1.4.13 compliance: tooltip must be dismissible (`Escape`), hoverable (pointer can move onto tooltip), and persistent until hover/focus leaves or dismissed.
- Upfront screen-reader wiring: every row's breakdown must be wired via `aria-describedby` at render time (not lazily on hover/focus).
- UI/UX Pro Max standards: native `<button type="button">` for clickable counts (never clickable `<span>`/`<div>`), ≥24px CSS hit height with clear hover/focus states (`cursor: pointer`, `:focus-visible` ring), `touch-action: manipulation`, and clear inline error feedback near the page header in `#browser`.
- Checks gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and headless Chrome screenshots per `REVIEW.md`.

## Review Focus

1. **Zero-page source (`0` active, `0` stale, `0` error):** Cell has no clickable count button, yet hovering or tabbing onto it still opens `#pages-tooltip` with `0 active pages · 0 stale (...) · 0 errors (...)`.
2. **Singular/plural & formatted numbers:** `1 active page`, `1 error`, `1,250 active pages`, `0 errors` — no `1 errors` or unformatted numbers.
3. **Viewport/overflow edge clipping:** First row near topbar, last row near viewport bottom, and horizontally scrolled table never clip `#pages-tooltip`; tooltip flips above/below and clamps horizontally within viewport padding.
4. **Hover bridge, `Escape`, and scroll hygiene:** Moving pointer from trigger to `#pages-tooltip` keeps it open; `Escape` hides it without blurring focus; scrolling hides it so it never floats detached.
5. **Sticky filter reset & null error fields:** Clicking `Browse` (without a status count) resets `browser-status-select` to `""`; an `error` page with `null` `error_message` or `null` `http_status` renders cleanly without `"null"`/`"undefined"`.

---

### Task 1: `Pages (active)` Column Markup, Accessible Breakdown, and Top-Layer Tooltip

**Files:**
- Modify: `src/ui/assets/index.html:128-146` and `src/ui/assets/index.html:310-316`
- Modify: `src/ui/assets/style.css` (append Pages cell & tooltip rules before responsive section)
- Modify: `src/ui/assets/app.js:310-358` and `src/ui/assets/app.js:1044-1056`
- Test: `src/ui/mod.rs` (add embedded-asset contract test)

**Interfaces:**
- Produces in `src/ui/assets/index.html`:
  - Column header `<th class="num">Pages (active)</th>`
  - Shared top-layer element `<div id="pages-tooltip" class="pages-tooltip" popover="hint" role="tooltip"></div>`
- Produces in `src/ui/assets/app.js`:
  - `formatPagesBreakdown(source)` -> `string`
  - `renderPagesCell(source)` -> `string` (HTML for `<td class="num pages-cell">...</td>`)
  - `initPagesTooltip()` -> wires hover/focus/Escape/scroll on `#sources-tbody` and `#pages-tooltip`
  - Clickable count buttons with `data-action="browse-status"`, `data-id="${s.id}"`, `data-status="active"|"stale"|"error"` (consumed by Task 2)

- [ ] **Step 1: Write the failing embedded-asset contract test in `src/ui/mod.rs`**

Add `ui_assets_include_pages_tooltip_and_accessible_header` to `mod tests` in `src/ui/mod.rs`:
- Fetch `/` (`index.html`) and assert it contains `Pages (active)`, `id="pages-tooltip"`, `popover="hint"`, and `role="tooltip"`.
- Fetch `/app.js` and assert it contains `formatPagesBreakdown`, `aria-describedby`, `browse-status`, and `initPagesTooltip`.
- Fetch `/style.css` and assert it contains `.pages-tooltip` and `.pages-count-btn`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test ui::tests::ui_assets_include_pages_tooltip_and_accessible_header`
Expected: FAIL because `Pages (active)`, `pages-tooltip`, and `formatPagesBreakdown` do not exist yet.

- [ ] **Step 3: Update `src/ui/assets/index.html`**

- Change `<th class="num">Pages</th>` to `<th class="num">Pages (active)</th>`.
- Right before `<!-- Add Source Modal -->`, add:
  ```html
  <div id="pages-tooltip" class="pages-tooltip" popover="hint" role="tooltip"></div>
  ```

- [ ] **Step 4: Implement `formatPagesBreakdown`, `renderPagesCell`, and `initPagesTooltip` in `src/ui/assets/app.js`**

- `formatPagesBreakdown(s)`:
  - Normalize `active = Number(s.active_pages || 0)`, `stale = Number(s.stale_pages || 0)`, `errors = Number(s.error_pages || 0)`.
  - Return exact format:
    `${formatNumber(active)} active ${active === 1 ? "page" : "pages"} · ${formatNumber(stale)} stale (no longer fetched, last good copy kept) · ${formatNumber(errors)} ${errors === 1 ? "error" : "errors"} (last fetch failed)`
- `renderPagesCell(s)`:
  - Compute `breakdown = formatPagesBreakdown(s)` and `descId = "pages-desc-" + s.id`.
  - Render `<td class="num pages-cell" data-pages-tooltip="${escapeHtml(breakdown)}">` containing:
    - Upfront `<span id="${descId}" class="sr-only">${escapeHtml(breakdown)}</span>`.
    - Active count: if `active > 0`, `<button type="button" class="pages-count-btn pages-count-active" data-action="browse-status" data-id="${s.id}" data-status="active" aria-describedby="${descId}">${formatNumber(active)}</button>`; else if `stale + errors === 0`, `<span class="pages-zero-trigger" tabindex="0" aria-describedby="${descId}">0</span>`; else `<span>0</span>`.
    - When `stale + errors > 0`, render `<span class="muted-meta pages-meta">(` + stale part (`stale > 0` -> `<button type="button" class="pages-count-btn pages-count-stale" data-action="browse-status" data-id="${s.id}" data-status="stale" aria-describedby="${descId}">${formatNumber(stale)}s</button>`, else `<span>0s</span>`) + ` / ` + error part (`errors > 0` -> `<button type="button" class="pages-count-btn pages-count-error" data-action="browse-status" data-id="${s.id}" data-status="error" aria-describedby="${descId}">${formatNumber(errors)}e</button>`, else `<span>0e</span>`) + `)</span>`.
- `initPagesTooltip()`:
  - Locate `#pages-tooltip` and `#sources-tbody`.
  - Feature-detect `popover="hint"`: probe `const p = document.createElement("div"); p.popover = "hint";` — if `p.popover !== "hint"`, set `tooltip.setAttribute("popover", "manual")` so Safari/older browsers still promote it cleanly to the top layer.
  - Manage a `hideTimer` (120ms close delay so pointer can cross into `#pages-tooltip` without closing — WCAG 1.4.13 hoverable).
  - Show on `mouseover`/`focusin` on any `[data-pages-tooltip]` cell (delegated on `#sources-tbody`), setting `tooltip.textContent = cell.getAttribute("data-pages-tooltip")`, calling `tooltip.showPopover()` (guarded with `try/catch` and `:popover-open` check), and positioning via `positionPagesTooltip(tooltip, anchorEl)`:
    - Measure `anchorRect` and `tooltip.getBoundingClientRect()`.
    - Default above (`anchorRect.top - tipRect.height - 8`); flip below (`anchorRect.bottom + 8`) if top `< 8`.
    - Align horizontally to anchor center and clamp `left` to `[8, window.innerWidth - tipRect.width - 8]`.
  - Keep open on `mouseenter` over `#pages-tooltip`; schedule hide on `mouseleave` / `focusout` (ignoring focus moves within the same cell).
  - Immediately hide on `Escape` (`keydown` on `document`) and on `scroll` (capture phase) or `resize`.
- Call `initPagesTooltip()` from `boot()`.

- [ ] **Step 5: Style `.pages-cell`, `.pages-count-btn`, `.pages-zero-trigger`, and `.pages-tooltip` in `src/ui/assets/style.css`**

- `.pages-cell`: `white-space: nowrap;`
- `.pages-count-btn`: inline-flex, `align-items: center`, `min-height: 24px`, `padding: 1px 5px`, `border-radius: 4px`, `border: 1px solid transparent`, `background: transparent`, `font: inherit`, `color: inherit`, `cursor: pointer`, `touch-action: manipulation`, subtle dotted bottom border or hover pill (`background: var(--accent-soft); color: var(--accent)` for active, `var(--warn-bg)` / `var(--warn)` for `.pages-count-stale`, `var(--err-bg)` / `var(--err-text)` for `.pages-count-error`), clear `:focus-visible` ring.
- `.pages-meta`: `display: inline-flex; align-items: center; gap: 2px; margin-left: 4px;` (compact spacing on desktop, `gap: 4px` on `<= 640px`).
- `.pages-zero-trigger`: `display: inline-flex; align-items: center; min-height: 24px; padding: 1px 4px; border-radius: 4px; border-bottom: 1px dotted var(--border-strong); cursor: help;`
- `.pages-tooltip`: top-layer popover reset (`position: fixed; inset: auto; margin: 0;`), `max-width: min(360px, calc(100vw - 16px))`, `padding: 8px 11px`, `border-radius: var(--radius-sm)`, `border: 1px solid var(--border-strong)`, `background: var(--surface)`, `color: var(--text)`, `box-shadow: var(--shadow-md)`, `font-family: var(--font-sans)`, `font-size: 12.5px`, `line-height: 1.45`, `white-space: normal`, `text-align: left`.

- [ ] **Step 6: Run tests to verify Task 1 passes**

Run: `cargo test ui::tests`
Expected: PASS (including `ui_assets_include_pages_tooltip_and_accessible_header` and `assets_contain_zero_external_cdn_links`).

- [ ] **Step 7: Commit Task 1**

```bash
git add src/ui/assets/index.html src/ui/assets/style.css src/ui/assets/app.js src/ui/mod.rs
git commit -m "feat(ui): add accessible top-layer tooltip to Sources Pages column (#7)"
```

---

### Task 2: Clickable Drill-Down into `#browser` & Error/Stale Detail Banners

**Files:**
- Modify: `src/ui/assets/app.js:173-201`, `src/ui/assets/app.js:359-372`, `src/ui/assets/app.js:534-544`, `src/ui/assets/app.js:639-725`
- Modify: `src/ui/assets/style.css` (detail status banner spacing)
- Test: `src/ui/mod.rs` (extend embedded-asset contract test for error banner & hash query plumbing)

**Interfaces:**
- Consumes from Task 1: `button[data-action="browse-status"][data-id][data-status]` in `#sources-tbody`
- Produces in `src/ui/assets/app.js`:
  - `parseHashParams()` -> `URLSearchParams` parsed from `window.location.hash` query string
  - `navigateToBrowser(sourceId, status)` -> updates `#browser` filters and navigates to `#browser?source=...&status=...` (omitting empty params)
  - `renderPageStatusBanner(page)` -> HTML string for `.detail-status-banner` (error alert with `http_status` + `error_message`, or warn alert for `stale` pages, or `""` for `active`)

- [ ] **Step 1: Extend the contract test in `src/ui/mod.rs`**

Add assertions to `ui_assets_include_pages_tooltip_and_accessible_header` (or add `ui_assets_include_browser_error_banner_and_drilldown`) checking `/app.js` contains `renderPageStatusBanner`, `navigateToBrowser`, and `error_message`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test ui::tests::ui_assets_include_browser_error_banner_and_drilldown`
Expected: FAIL because `renderPageStatusBanner` and `navigateToBrowser` do not exist yet.

- [ ] **Step 3: Implement hash-param drill-down and filter reset in `src/ui/assets/app.js`**

- Add `parseHashParams()`: extracts query string after `?` in `window.location.hash` and returns `new URLSearchParams(...)`.
- Add `navigateToBrowser(sourceId, status)`:
  - Builds `params = new URLSearchParams()`; sets `source` if `sourceId` is non-empty and `status` if `status` is `"active" | "stale" | "error"`.
  - Sets `browser-search-input` to `""`, `browser-source-select` to `String(sourceId || "")`, `browser-status-select` to `status || ""`.
  - Sets `window.location.hash = params.toString() ? "#browser?" + params.toString() : "#browser"`.
  - If already on `#browser`, calls `void loadBrowserView()` directly so clicking still refreshes.
- Update `handleSourcesTableClick`:
  - `action === "browse"` calls `navigateToBrowser(id, "")` (explicitly resetting any sticky status filter!).
  - `action === "browse-status"` reads `status = btn.getAttribute("data-status") || ""` and calls `navigateToBrowser(id, status)`.
- Update `loadBrowserView()`:
  - After ensuring `state.sources` is loaded (and `populateBrowserSourceSelect()` has run), read `parseHashParams()`:
    - If `source` param is present and matches an option, set `browser-source-select.value`.
    - If `status` param is present (`active`, `stale`, `error`), set `browser-status-select.value`.
  - Then call `await searchBrowserPages()`.

- [ ] **Step 4: Implement `renderPageStatusBanner(page)` and list HTTP badge in `src/ui/assets/app.js` & `src/ui/assets/style.css`**

- `renderPageStatusBanner(page)`:
  - If `page.status === "error"` or `page.error_message`:
    - Build parts cleanly without `"null"`/`"undefined"`: prefix `HTTP ${page.http_status}` when `page.http_status !== null && page.http_status !== undefined`; message `page.error_message` when non-empty; fallback `"Last fetch failed."` when both are absent.
    - Return `<div class="alert alert-error detail-status-banner" role="alert"><strong>Fetch error:</strong> ${escapeHtml(text)}</div>`.
  - Else if `page.status === "stale"`:
    - Return `<div class="alert alert-warn detail-status-banner" role="status"><strong>Stale page:</strong> No longer fetched on the last sync; last good copy is kept.</div>`.
  - Else return `""`.
- Insert `${renderPageStatusBanner(page)}` inside `.detail-header` in `renderPageDetail(page)`.
- In `renderBrowserPageList()`: when `p.status === "error"` and `p.http_status != null`, include `<span class="mono">HTTP ${escapeHtml(p.http_status)}</span>` inside `.page-item-meta`.
- In `src/ui/assets/style.css`: add `.detail-status-banner { margin-top: var(--space-1); }`.

- [ ] **Step 5: Run tests to verify Task 2 passes**

Run: `cargo test ui::tests`
Expected: PASS.

- [ ] **Step 6: Commit Task 2**

```bash
git add src/ui/assets/app.js src/ui/assets/style.css src/ui/mod.rs
git commit -m "feat(ui): drill down from Pages counts to Browser with error/stale banners (#7)"
```

---

### Task 3: Full Cargo Gate & Headless Chrome Visual/Interactive Verification

**Files:**
- Verify only (no new product files; fix any visual or behavioral defect found in `src/ui/assets/*` and re-run)

- [ ] **Step 1: Run the full Rust check gate**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
Expected: 0 warnings, all unit and integration tests PASS.

- [ ] **Step 2: Seed an isolated temp vault with active, stale, error, and 0-page sources**

Create `/tmp/wv-ui-review`, add a local Markdown source, insert realistic `stale` and `error` rows (with `http_status = 404` and `error_message = "HTTP 404 Not Found"`) plus a 0-page source directly via `sqlite3`, and launch `citadel ui --port 4317 --no-open`.

- [ ] **Step 3: Run headless Chrome DOM & screenshot verification (`REVIEW.md` loop)**

Capture and inspect via `read_file`:
- `/tmp/wv-sources.png` (1440×900 light mode — verify `Pages (active)` header, active/stale/error counts, no layout break)
- `/tmp/wv-sources-tooltip.png` (1440×900 with tooltip visible — verify crisp contrast, no `.panel` clipping)
- `/tmp/wv-sources-dark.png` (1440×900 dark mode — verify contrast on counts and tooltip)
- `/tmp/wv-mobile.png` (480×900 mobile — verify touch spacing & no overflow)
- `/tmp/wv-browser-error.png` (1440×900 `#browser?source=1&status=error` — verify pre-filtered error list + `Fetch error: HTTP 404 — ...` banner in detail pane)

- [ ] **Step 4: Clean up temp vault and stop UI server**

```bash
kill "$UI_PID" 2>/dev/null || true
rm -rf /tmp/wv-ui-review /tmp/wv-ui.log /tmp/wv-*.png
```

Expected: Clean workspace (`git status` clean), all evidence ready for `HumanAudit` handoff.
