# Design: Sources Table `Pages` Column Tooltip & Drill-Down (Issue #7)

## Intent & Why
The Sources table currently shows values like `19 (2s / 0e)` with no explanation that the main number counts only **active** pages or what `s` (stale) and `e` (error) mean. Making the breakdown self-explanatory on hover/focus/screen-reader and letting users click directly into affected pages removes guesswork and speeds up debugging failed or stale fetches.

## Scope & Size
**Small change** — frontend-only (`src/ui/assets/index.html`, `src/ui/assets/style.css`, `src/ui/assets/app.js`) plus headless Chrome visual verification per `REVIEW.md`. No backend or schema changes needed: `/api/sources/{id}/pages` and `/api/pages/{id}` already return `http_status` and `error_message`.

## Goals, Non-Goals, Anti-Goals
- **Goals:**
  - Explain `active`, `stale`, and `error` counts (and what each status means) on hover and keyboard focus for every row in the Sources table.
  - Expose the full breakdown to screen readers upfront (browse-mode friendly, not lazy-wired on focus).
  - Make non-zero counts clickable to jump straight to `#browser` pre-filtered to that source and status, and display `http_status` / `error_message` in the Knowledge Browser detail view.
  - Rename the column header from `Pages` to `Pages (active)`.
- **Non-Goals:**
  - Adding a separate modal/drawer or modifying the `Changes & Health` view.
  - Changing the SQLite schema, REST API payloads, or adding any external JS/CSS dependencies.
- **Anti-Goals:**
  - Tooltips clipped by `.panel { overflow: hidden }` or `.table-wrap { overflow-x: auto }`.
  - Hover-only tooltips that fail keyboard users or violate WCAG 1.4.13 (not hoverable or not `Escape`-dismissible).

## Constraints
- **Zero-runtime-footprint:** Pure embedded HTML/CSS/JS via `rust-embed`; no CDN or third-party tooltip libraries.
- **Cross-browser without polyfills:** Must work in Chrome, Firefox, and Safari without loading external polyfills.
- **WCAG 1.4.13:** Tooltip must be dismissible (`Escape`), hoverable (pointer can move onto it), and persistent until hover/focus leaves or user dismisses.
- **Verification:** Visual check with headless Chrome screenshots (light, dark, mobile, and tooltip-open state) per `REVIEW.md`.

## Approaches Considered
- **Approach A — Clickable counts + shared top-layer tooltip *(Picked)*:** Each non-zero count (`active`, `stale`, `error`) is a compact button that jumps to `#browser` pre-filtered to `(source, status)`. Hovering or focusing any `Pages` cell opens a single shared top-layer `[popover]` tooltip with the full breakdown. *Trade-offs:* 1-click drill-down on desktop and matches issue #7 directly; touch taps navigate immediately rather than previewing the tooltip (mitigated by the `Pages (active)` header and status badges in `#browser`).
- **Approach B — Single cell button → interactive breakdown popover:** Clicking/hovering/tapping the cell opens a popover card listing all three statuses with drill-down links. *Trade-offs:* Uniform touch behavior and 1 tab stop per row, at the cost of an extra click on desktop and heavier popover interaction.

**Why A:** Directly fits issue #7's mental model, keeps the Sources table fast (1-click drill-down), and stays minimal in vanilla JS/CSS.

## Design Decisions
- **User decided:** Include clickable drill-down from `Pages` counts into `#browser` with `error_message` surfaced in the detail pane; use Approach A.
- **Proposed calls (confirm or adjust):**
  - **Column header:** Rename `Pages` to `Pages (active)` in `src/ui/assets/index.html`.
  - **Cell structure (`src/ui/assets/app.js`):**
    - Every row's `Pages` cell has a breakdown sentence, e.g. `19 active pages · 2 stale (no longer fetched, last good copy kept) · 0 errors (last fetch failed)`.
    - Non-zero counts (`active`, `stale`, `error`) render as `<button type="button" class="pages-count-btn" data-action="browse-status" data-id="..." data-status="...">`; zero counts (and rows with `0` active pages) render as non-clickable text inside a focusable wrapper (`tabindex="0"`) when needed so every row can still trigger the tooltip via keyboard.
    - Each row includes an upfront `<span id="pages-desc-${id}" class="sr-only">` referenced by `aria-describedby` on the cell's interactive trigger(s), so browse-mode screen readers always read the exact per-row breakdown without needing hover/focus first.
  - **Shared top-layer tooltip:** One `<div id="pages-tooltip" popover="hint" role="tooltip">` (falling back to `popover="manual"` where `"hint"` is unsupported) controlled by a small helper in `app.js`:
    - Opens on `mouseenter` / `focusin`, stays open while pointer is over the trigger or tooltip (small close delay for moving pointer onto the tooltip), closes on `mouseleave` / `focusout` / `Escape` / scroll.
    - Positioned via `getBoundingClientRect()` (flipping above/below and clamping to viewport) so it works identically in Chrome, Firefox, and Safari with zero polyfills and zero overflow clipping.
  - **Knowledge Browser drill-down & error display:**
    - Clicking a count sets `browser-source-select`, `browser-status-select`, clears `browser-search-input`, and switches to `#browser`.
    - In `renderPageDetail()`, when `page.error_message` or a failing `page.http_status` is present, render an `.alert.alert-error` banner below the detail header metadata showing the HTTP status and `error_message`. For `stale` pages, render a compact `.alert.alert-warn` note explaining that the page was not fetched on the last sync and the last good copy is kept.

## Left to the Builder
- Exact CSS styling for `.pages-count-btn` (subtle hover/focus affordance, tabular numerals, light/dark contrast) and `#pages-tooltip` (matching existing `--surface`, `--border`, `--shadow-md` tokens and `prefers-reduced-motion`).
- Whether to reflect `source` and `status` in the `#browser` URL hash query (e.g. `#browser?source=1&status=error`) so reload/back preserves the drill-down filter.
- Headless Chrome verification script/screenshots covering light mode, dark mode, mobile, tooltip-open state, and `#browser` error banner.
