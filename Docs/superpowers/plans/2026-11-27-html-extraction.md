# HTML Extraction Fix (Issue #6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `extract_page_markdown_and_title` in `src/ingest/web/collected.rs` so short documentation pages store their `<main>`/`[role="main"]`/`<article>` content instead of sidebar or dialog widgets, and non-landmark pages fall back when Readability drops the page's main heading.

**Architecture:** Pre-filter raw HTML with `lol_html` to remove `nav`, `footer`, `script`, `style`, `noscript`, `title`, `aside`, `dialog`, `[role="complementary"]`, and `[role="dialog"]` before `spider_transformations::transform_content` runs. If the cleaned HTML contains a non-empty `<main>`, `[role="main"]`, or single `<article>`, convert that landmark directly (`readability: false`); otherwise run Readability + `should_prefer_fallback` enhanced with a `<title>` / `<h1>` heading check.

**Tech Stack:** Rust, `lol_html` 2, `spider_transformations` 2.39, `spider` 2.54.

**Spec:** [Docs/superpowers/specs/2026-11-27-html-extraction-design.md](../specs/2026-11-27-html-extraction-design.md)

## Global Constraints
- `spider_transformations::transform_content` runs `llm_readability` on raw HTML *before* `ignore_tags` is applied, and `ignore_tags` only matches tag names (not CSS attribute selectors). Pre-filtering and landmark extraction must happen before `transform_content` using `lol_html`.
- Propagate or handle errors without `unwrap()`/`expect()` outside `#[cfg(test)]`.
- Follow TDD (failing test first) and pass `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Keep this change strictly focused on HTML extraction (no Sync-level duplicate-hash warnings).

## Review Focus
1. **Empty `<main>` with non-empty `<article>`**: A page with `<main></main>` (or `<main>` whose only content was a stripped `<nav>`) and a real `<article>` must skip the empty `<main>` and extract `<article>`.
2. **Nested landmarks (`<main>` containing `<article>`)**: Selecting `<main>` must keep the entire `<main>` subtree including any nested `<article>`, not truncate at the inner `</article>`.
3. **Multiple `<article>` cards without `<main>`**: A listing page with 2+ `<article>` elements and no `<main>`/`[role="main"]` must not be truncated to only the first `<article>`; it should fall through to the non-landmark path.
4. **Widget nested inside `<main>`**: An `<aside>` or `<div role="dialog">` placed *inside* `<main>` must still be stripped before Markdown conversion.
5. **Non-landmark page where Readability drops the `<h1>`/`<title>` heading**: A page without landmarks where Readability picks a longer `<div>` widget lacking the `<title>`/`<h1>` heading must fall back to `fallback_md` (which retains the heading).

---

### Task 1: Pre-strip non-content elements and convert semantic landmarks directly

**Files:**
- Modify: `src/ingest/web/collected.rs:700-740` (production) and `src/ingest/web/collected.rs:866+` (tests)

**Interfaces:**
- Produces:
  - `fn strip_non_content_html(html: &str) -> String`
  - `fn extract_landmark_html(cleaned_html: &str) -> Option<String>`
  - Updated `fn extract_page_markdown_and_title(page: &Page, url: &Url) -> (String, Option<String>)`

- [ ] **Step 1: Write the failing tests in `src/ingest/web/collected.rs`**

Add a `mod extraction_tests` inside `mod tests` in `src/ingest/web/collected.rs` covering:
- `short_main_card_grid_beats_longer_aside_and_dialog_widgets`: HTML with `<main><h1>Destinations</h1><ul><li><a href="/big">BigQuery</a></li></ul></main>`, a 150-word `<aside>` ("DHelp..."), a `<div role="dialog">...</div>`, a `<div role="complementary">...</div>`, and a `<dialog>...</dialog>`. Assert `extract_page_markdown_and_title` output contains `"Destinations"` and `"BigQuery"` and does not contain `"DHelp"` or the dialog/complementary text.
- `empty_main_falls_through_to_article`: HTML with `<main><nav>Menu</nav></main><article><h1>Guide</h1><p>Body.</p></article>`. Assert output contains `"Guide"` and `"Body."` and not `"Menu"`.
- `nested_article_inside_main_is_preserved`: HTML with `<main><h1>Overview</h1><article><h2>Card</h2><p>Details.</p></article></main>`. Assert output contains both `"Overview"` and `"Details."`.
- `multiple_articles_without_main_are_not_truncated_to_first`: HTML with `<article><h2>First</h2><p>One.</p></article><article><h2>Second</h2><p>Two.</p></article>` (no `<main>`). Assert output contains both `"First"` and `"Second"`.
- `widget_inside_main_is_stripped`: HTML with `<main><h1>Page</h1><p>Real.</p><aside><p>Widget.</p></aside><div role="dialog"><p>Popup.</p></div></main>`. Assert output contains `"Real."` and does not contain `"Widget."` or `"Popup."`.

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib ingest::web::collected::tests::extraction_tests`
Expected: FAIL on `short_main_card_grid_beats_longer_aside_and_dialog_widgets` (and `widget_inside_main_is_stripped`).

- [ ] **Step 3: Implement `strip_non_content_html`, `extract_landmark_html`, and wire into `extract_page_markdown_and_title`**

In `src/ingest/web/collected.rs`:
- Implement `strip_non_content_html(html: &str) -> String` using `lol_html::rewrite_str` to call `el.remove()` on `nav`, `footer`, `script`, `style`, `noscript`, `title`, `aside`, `dialog`, `[role="complementary"]`, `[role="dialog"]` (falling back to `html.to_string()` if rewriting errors).
- Implement `extract_landmark_html(cleaned_html: &str) -> Option<String>` using `lol_html` (or a small depth-tracking pass / `lol_html` element + text handlers) to find candidates in priority order: `<main>`, `[role="main"]`, and `<article>` (only selecting `<article>` when there is at most one non-empty `<article>`). Return `Some(inner_or_outer_html)` for the first non-empty candidate (has non-whitespace text).
- In `extract_page_markdown_and_title`:
  1. Extract `html_title` from `raw_content` before stripping `<title>`.
  2. Call `let cleaned_html = strip_non_content_html(&raw_content);`
  3. If `let Some(landmark_html) = extract_landmark_html(&cleaned_html)`, build a `Page` from `landmark_html` (preserving URL/headers if needed, or `Page::default()` with `set_url` + `set_html_bytes(Some(landmark_html.into_bytes()))`) and convert with `readability: false`, `ignore_tags: None`.
  4. Otherwise, build a `Page` from `cleaned_html` and run the Readability + fallback path.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib ingest::web::collected`
Expected: PASS (all existing + new `extraction_tests`).

- [ ] **Step 5: Commit**

```bash
git add src/ingest/web/collected.rs
git commit -m "fix(ingest): pre-strip widgets and prefer semantic landmarks in HTML extraction"
```

---

### Task 2: Add `<title>` / `<h1>` heading sanity check to `should_prefer_fallback`

**Files:**
- Modify: `src/ingest/web/collected.rs:700-760` (production) and `src/ingest/web/collected.rs` (`extraction_tests`)

**Interfaces:**
- Consumes: `strip_non_content_html`, `extract_html_title` from `src/ingest/web/collected.rs`
- Produces:
  - `fn extract_html_h1(html: &str) -> Option<String>`
  - `fn has_matching_heading(md: &str, html_title: Option<&str>, html_h1: Option<&str>) -> bool`
  - Updated `fn should_prefer_fallback(readability_md: &str, fallback_md: &str, html_title: Option<&str>, html_h1: Option<&str>) -> bool`

- [ ] **Step 1: Write the failing tests in `src/ingest/web/collected.rs`**

Add to `extraction_tests`:
- `non_landmark_page_prefers_fallback_when_readability_drops_matching_heading`: HTML with `<head><title>Destinations | Docs</title></head><body><div><h1>Destinations</h1><p>Short list.</p></div><div class="widget"><p>...(150 words of unrelated widget prose)...</p></div></body>` (no `<main>`/`<article>`). Assert `extract_page_markdown_and_title` output contains `"# Destinations"` and `"Short list."`.
- `should_prefer_fallback_keeps_readability_when_matching_heading_present`: Unit test on `should_prefer_fallback` where both `readability_md` and `fallback_md` contain `# Destinations`, and `readability_md` is at least 1/4 of `fallback_md` length. Assert `should_prefer_fallback` returns `false`.

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib ingest::web::collected::tests::extraction_tests`
Expected: FAIL on `non_landmark_page_prefers_fallback_when_readability_drops_matching_heading`.

- [ ] **Step 3: Implement `extract_html_h1`, `has_matching_heading`, and update `should_prefer_fallback`**

In `src/ingest/web/collected.rs`:
- `extract_html_h1(html: &str) -> Option<String>`: extract text of the first `<h1>` in the cleaned HTML (stripping inner tags, decoding basic entities, collapsing whitespace).
- `has_matching_heading(md: &str, html_title: Option<&str>, html_h1: Option<&str>) -> bool`: scan Markdown heading lines (`# `, `## `, `### `), normalize case and whitespace, and return `true` if any heading matches `html_h1` or is a non-trivial prefix/substring match of `html_title` (e.g., `"Destinations"` matching `"Destinations | Docs"`).
- In `should_prefer_fallback`: if `!has_matching_heading(readability_md, html_title, html_h1) && has_matching_heading(fallback_md, html_title, html_h1)`, return `true`.

- [ ] **Step 4: Run tests and full checks gate**

Run:
```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
Expected: All checks and tests pass with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/ingest/web/collected.rs
git commit -m "fix(ingest): prefer fallback when Readability drops page title or h1 heading"
```

---

### Task 3: Live spot-check against `dlthub` and 3 other docs generators (Issue #6 acceptance criteria)

**Files:**
- No code changes expected (unless a spot-check reveals an edge case, which gets its own failing test first).

- [ ] **Step 1: Verify `dlthub` re-sync**

Build release/debug binary and re-sync `dlthub` (or run against the local vault) to confirm:
- No non-empty `content_hash` is shared by more than 2 pages on `dlthub`.
- The 12 previously affected `dlthub.com/docs/` URLs store their own content instead of the "DHelp" Codespaces widget.

- [ ] **Step 2: Spot-check 3 other docs generators**

Test extraction on at least 3 other web Sources built with different docs generators (e.g., Docusaurus, MkDocs/Material, Sphinx, VitePress, Mintlify) and record the results for the PR / HumanAudit handoff.

- [ ] **Step 3: Final checks gate**

Run:
```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
Expected: Exit code 0 across all three commands.
