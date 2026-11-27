# HTML Extraction Fix for Short Documentation Pages (Issue #6)

## Intent & Why
On short documentation pages (card grids, link lists, nearly empty API module pages), `extract_page_markdown_and_title` can store a sidebar or dialog widget instead of the page's actual content—for example, 12 `dlthub` pages storing a 130-word Codespaces help widget under the same `content_hash`. Fixing HTML extraction ensures `query` returns real page content for short pages without requiring `render_js: true`.

## Goals, Non-Goals, Anti-Goals
- **Goals**:
  - Store `<main>`, `[role=main]`, or `<article>` content when present on a page, even when it is shorter than surrounding widgets.
  - Strip non-content elements (`aside`, `dialog`, `[role=complementary]`, `[role=dialog]`, `nav`, `footer`, `script`, `style`, `noscript`, `title`) before Readability or Markdown conversion runs.
  - Catch cases on non-landmark pages where Readability drops the page's main heading (`<title>` or `<h1>`) in favor of a widget.
- **Non-Goals**:
  - Sync-level duplicate `content_hash` warnings (explicitly excluded to keep this change focused on HTML extraction).
  - Requiring `render_js: true` for static documentation sites.
- **Anti-Goals**:
  - Degrading extraction quality on long prose pages or pages without semantic landmarks.

## Constraints
- `spider_transformations::transform_content` runs `llm_readability` on raw HTML *before* `ignore_tags` is applied, and `ignore_tags` only matches tag names (not CSS attribute selectors). Pre-filtering and landmark extraction must therefore happen before `transform_content`, using `lol_html` (already a direct dependency).
- Follow TDD (`Red first`) and the `AGENTS.md` checks gate (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`).

## Approaches Considered
1. **Approach A — Pre-strip widgets, convert semantic landmarks directly (Picked)**
   - Strip `nav`, `footer`, `script`, `style`, `noscript`, `title`, `aside`, `dialog`, `[role=complementary]`, `[role=dialog]` with `lol_html`. If `<main>`, `[role=main]`, or `<article>` exists and is non-empty, convert it directly (`readability: false`); otherwise run Readability + `should_prefer_fallback` with a `<title>`/`<h1>` heading check.
   - *Trade-off*: Predictable and fast on modern docs sites; guarantees short card grids/API stubs in `<main>` are never dropped by Readability, at the small risk of keeping non-`aside` chrome if a site places it inside `<main>`.
2. **Approach B — Pre-strip widgets, always run Readability + heading check**
   - Same `lol_html` pre-strip, then always run Readability + fallback.
   - *Trade-off*: Smaller shift for messy `<main>` pages, but Readability can still drop short `<main>` card grids/lists if `<title>`/`<h1>` isn't matched.
3. **Approach C — Scope to landmark, strip widgets, and still run Readability + heading check**
   - Isolate landmark when present, strip widgets, then run both Readability and fallback inside it.
   - *Trade-off*: Cleans extra non-`aside` chrome inside `<main>`, but still runs two passes and relies on `should_prefer_fallback` heuristics to rescue short `<main>` pages.

**Why Approach A**: Most documentation generators wrap their body in `<main>`, `[role=main]`, or `<article>`. Converting that cleaned landmark directly eliminates Readability's length bias on short pages while preserving Readability + heading-aware fallback for pages without landmarks.

## Decided Design (`src/ingest/web/collected.rs`)
1. **Pre-strip noisy elements**: Before calling `transform_content`, run a `lol_html` pass removing `nav`, `footer`, `script`, `style`, `noscript`, `title`, `aside`, `dialog`, `[role="complementary"]`, and `[role="dialog"]`.
2. **Landmark selection**: On the cleaned HTML, look for `<main>`, then `[role="main"]`, then `<article>`. If the first matching landmark has non-empty text, convert its HTML directly to Markdown (`readability: false`).
3. **Non-landmark fallback + heading sanity check**: When no non-empty landmark exists, run Readability (`readability: true`) and fallback (`readability: false`) on the cleaned HTML. In `should_prefer_fallback`, also prefer fallback if Readability's output has no heading matching the page's `<title>` or `<h1>` while the fallback does.

## Left to the Builder
- Exact `lol_html` implementation for stripping elements and extracting the first non-empty landmark subtree.
- Exact heading-matching normalization (case/whitespace trimming) against `<title>` / `<h1>`.
- Writing the failing test first (short `<main>` card grid + longer `<aside>`/`dialog` widget), verifying existing extraction tests pass, and spot-checking `dlthub` plus at least 3 other docs generators per Issue #6 acceptance criteria.
