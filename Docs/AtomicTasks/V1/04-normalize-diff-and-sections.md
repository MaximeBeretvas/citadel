# Task 04: Content Normalization, Diffing & Heading Section Splitter

- **Phase:** 1 (Prototype Vertical Slice)
- **Dependencies:** [01-project-scaffold.md](./01-project-scaffold.md)
- **Suggested Commit:** `feat(ingest): add Markdown normalizer, SHA-256 diffing, and H1-H3 section splitter`

## Goal
Implement the pure, deterministic text-processing core for ingestion: volatile frontmatter stripping, line-ending normalization, SHA-256 content hashing, capped unified diffs (`similar`), and `pulldown-cmark` H1–H3 section splitting with breadcrumb paths per [Docs/SPEC.md](../../SPEC.md#L247-L263).

## Exact Files
- `src/ingest/normalize.rs`
- `src/ingest/sections.rs`
- `src/ingest/mod.rs` (module exports)

## Red-First Test Cases (Write Before Implementation)
1. `normalize::tests::strips_volatile_frontmatter_keys_for_hashing`: two docs differing only in `created:`, `updated:`, `date:`, `crawled_at:`, `fetched_at:`, or `last_modified:` produce the exact same `content_hash`.
2. `normalize::tests::preserves_substantive_frontmatter_and_extracts_title`: extracts `title:` from YAML frontmatter (falling back to first H1, then filename stem).
3. `normalize::tests::normalizes_crlf_trailing_spaces_and_blank_lines`: `\r\n`, trailing spaces, and 4+ blank lines normalize cleanly.
4. `normalize::tests::generates_unified_diff_capped_at_64kb`: produces unified diff on real changes and truncates cleanly at 64 KB without splitting UTF-8 codepoints.
5. `sections::tests::splits_h1_h2_h3_with_breadcrumbs`: builds `ordinal`, `level`, `heading`, `heading_path` (e.g. `Data Modeling > Kimball > SCD Types`), `content_md`, and `word_count`; intro text before first heading gets `ordinal = 0`, `level = 0`, `heading = "(Intro)"`.
6. `sections::tests::ignores_hashes_inside_fenced_code_blocks`: `# comment` inside ```python``` does NOT split a section.

## Acceptance Criteria
- 100% pure functions with zero DB or network dependencies (fast unit tests).
- UTF-8-safe diff truncation at 64 KB.
- Zero `unwrap()` / `expect()` outside `#[cfg(test)]`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
