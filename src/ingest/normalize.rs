use std::fmt::Write as _;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use similar::TextDiff;

/// Maximum byte size retained for stored unified diffs (64 KB).
pub const MAX_DIFF_BYTES: usize = 64 * 1024;

const DIFF_TRUNCATION_SUFFIX: &str = "\n... [truncated at 64 KB]";

const VOLATILE_FRONTMATTER_KEYS: [&str; 6] = [
    "created",
    "updated",
    "date",
    "crawled_at",
    "fetched_at",
    "last_modified",
];

/// Normalized representation of an ingested Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDocument {
    pub title: String,
    pub content_md: String,
    pub content_hash: String,
    pub word_count: usize,
}

/// Parsed leading YAML frontmatter slice and the byte offset where Markdown body content begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct YamlFrontmatter<'a> {
    pub inner: &'a str,
    pub body_start: usize,
}

/// Normalize a raw Markdown document into clean Markdown, title, SHA-256 hash, and word count.
pub fn normalize_markdown(raw_md: &str, fallback_title: &str) -> NormalizedDocument {
    let stripped = strip_volatile_frontmatter(raw_md);
    let content_md = normalize_whitespace(&stripped);
    let title = extract_title(&content_md, fallback_title);
    let content_hash = sha256_hex(&content_md);
    let word_count = count_words(&content_md);

    NormalizedDocument {
        title,
        content_md,
        content_hash,
        word_count,
    }
}

/// Strip volatile timestamp/crawl keys from a leading YAML frontmatter block.
///
/// Drops the entire `---` wrapper and leading blank lines if only volatile keys were present.
pub fn strip_volatile_frontmatter(raw_md: &str) -> String {
    let Some(fm) = split_yaml_frontmatter(raw_md) else {
        return raw_md.to_string();
    };

    let mut kept: Vec<&str> = Vec::new();
    let mut skipping_volatile = false;

    for raw_line in fm.inner.lines() {
        let line = raw_line.trim_end_matches('\r');

        if let Some((key, _val)) = parse_yaml_top_key(line) {
            if is_volatile_key(key) {
                skipping_volatile = true;
            } else {
                skipping_volatile = false;
                kept.push(line);
            }
            continue;
        }

        if line.trim().is_empty() {
            if !skipping_volatile {
                kept.push("");
            }
            continue;
        }

        // Indented continuations or unindented YAML list items belong to the preceding key.
        let is_continuation =
            line.starts_with([' ', '\t']) || line.starts_with("- ") || line == "-";
        if skipping_volatile && is_continuation {
            continue;
        }

        skipping_volatile = false;
        kept.push(line);
    }

    while kept.first().is_some_and(|l| l.is_empty()) {
        kept.remove(0);
    }
    while kept.last().is_some_and(|l| l.is_empty()) {
        kept.pop();
    }

    let body = &raw_md[fm.body_start..];
    if kept.is_empty() {
        return body.trim_start_matches(['\r', '\n']).to_string();
    }

    let mut out = String::with_capacity(raw_md.len());
    out.push_str("---\n");
    for line in kept {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("---");
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    out
}

/// Normalize line endings, strip trailing horizontal whitespace per line, and collapse 3+ blank lines to 2.
pub fn normalize_whitespace(md: &str) -> String {
    let without_bom = strip_bom(md);
    let normalized_lf = without_bom.replace("\r\n", "\n").replace('\r', "\n");

    let mut lines: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;

    for raw_line in normalized_lf.split('\n') {
        let trimmed = raw_line.trim_end_matches([' ', '\t']);
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue;
            }
            blank_run += 1;
            if blank_run <= 2 {
                lines.push("");
            }
        } else {
            blank_run = 0;
            lines.push(trimmed);
        }
    }

    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

/// Extract document title from YAML frontmatter `title:`, falling back to the first H1 heading, then `fallback_stem`.
pub fn extract_title(raw_or_normalized_md: &str, fallback_stem: &str) -> String {
    let body = match split_yaml_frontmatter(raw_or_normalized_md) {
        Some(fm) => {
            if let Some(title) = extract_frontmatter_title(fm.inner) {
                return title;
            }
            &raw_or_normalized_md[fm.body_start..]
        }
        None => strip_bom(raw_or_normalized_md),
    };

    if let Some(h1) = extract_first_h1(body) {
        return h1;
    }

    fallback_stem.to_string()
}

/// Compute lowercase hexadecimal SHA-256 digest of `input`.
pub fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Count whitespace-delimited words in `text`.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Generate a line-based unified diff capped at [`MAX_DIFF_BYTES`] on a valid UTF-8 boundary.
pub fn unified_diff(old_md: &str, new_md: &str) -> String {
    if old_md == new_md {
        return String::new();
    }

    let diff = TextDiff::from_lines(old_md, new_md)
        .unified_diff()
        .header("previous", "current")
        .missing_newline_hint(false)
        .to_string();

    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }

    let budget = MAX_DIFF_BYTES.saturating_sub(DIFF_TRUNCATION_SUFFIX.len());
    let mut cut = budget.min(diff.len());
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }

    let mut truncated = String::with_capacity(cut + DIFF_TRUNCATION_SUFFIX.len());
    truncated.push_str(&diff[..cut]);
    truncated.push_str(DIFF_TRUNCATION_SUFFIX);
    truncated
}

pub(crate) fn split_yaml_frontmatter(md: &str) -> Option<YamlFrontmatter<'_>> {
    let bom_len = if md.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };

    let first_nl = md[bom_len..].find('\n')?;
    let line0_end = bom_len + first_nl;
    let line0 = md[bom_len..line0_end]
        .trim_end_matches('\r')
        .trim_end_matches([' ', '\t']);
    if line0 != "---" {
        return None;
    }

    let inner_start = line0_end + 1;
    let mut cursor = inner_start;

    while cursor < md.len() {
        let rel_nl = md[cursor..].find('\n');
        let line_end = rel_nl.map_or(md.len(), |idx| cursor + idx);
        let line = md[cursor..line_end].trim_end_matches('\r');
        let next_cursor = rel_nl.map_or(md.len(), |_| line_end + 1);

        if line.trim_end_matches([' ', '\t']) == "---" {
            let inner = &md[inner_start..cursor];
            if looks_like_yaml_frontmatter(inner) {
                return Some(YamlFrontmatter {
                    inner,
                    body_start: next_cursor,
                });
            }
            return None;
        }

        cursor = next_cursor;
    }

    None
}

pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn looks_like_yaml_frontmatter(inner: &str) -> bool {
    let mut has_content = false;
    for raw_line in inner.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        has_content = true;
        if parse_yaml_top_key(line).is_some() {
            return true;
        }
    }
    !has_content
}

fn parse_yaml_top_key(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() || line.starts_with([' ', '\t', '#', '-']) {
        return None;
    }

    let (raw_key, raw_val) = line.split_once(':')?;
    if !raw_val.is_empty() && !raw_val.starts_with([' ', '\t']) {
        return None;
    }

    let key = unquote_matching(raw_key.trim());
    if key.is_empty() {
        return None;
    }

    Some((key, raw_val.trim()))
}

fn is_volatile_key(key: &str) -> bool {
    VOLATILE_FRONTMATTER_KEYS
        .iter()
        .any(|volatile| key.eq_ignore_ascii_case(volatile))
}

fn unquote_matching(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn extract_frontmatter_title(inner: &str) -> Option<String> {
    for line in inner.lines() {
        let Some((key, val)) = parse_yaml_top_key(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("title") {
            let cleaned = unquote_matching(val.trim()).trim();
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

pub(crate) fn is_container_start(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::BlockQuote(_) | Tag::List(_) | Tag::Item | Tag::FootnoteDefinition(_) | Tag::Table(_)
    )
}

pub(crate) fn is_container_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::BlockQuote(_)
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
    )
}

pub(crate) fn append_heading_text(buf: &mut String, event: &Event<'_>) {
    match event {
        Event::Text(text) | Event::Code(text) => buf.push_str(text),
        Event::SoftBreak | Event::HardBreak => buf.push(' '),
        _ => {}
    }
}

fn extract_first_h1(body: &str) -> Option<String> {
    let mut container_depth = 0usize;
    let mut in_h1 = false;
    let mut title_buf = String::new();

    for event in Parser::new(body) {
        match &event {
            Event::Start(tag) if is_container_start(tag) => {
                container_depth += 1;
            }
            Event::End(tag_end) if is_container_end(tag_end) => {
                container_depth = container_depth.saturating_sub(1);
            }
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if container_depth == 0 => {
                in_h1 = true;
                title_buf.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if in_h1 => {
                in_h1 = false;
                let trimmed = title_buf.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
                title_buf.clear();
            }
            _ if in_h1 => append_heading_text(&mut title_buf, &event),
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_volatile_frontmatter_keys_for_hashing() {
        let doc_a = "\
---
title: Warehouse Guide
created: 2026-01-01T00:00:00Z
updated: 2026-02-01T10:20:30Z
date: 2026-02-01
crawled_at: 2026-03-01T08:00:00Z
fetched_at: 2026-03-01T08:00:05Z
last_modified: Wed, 01 Feb 2026 10:20:30 GMT
tags:
  - data
---

# Warehouse Guide

Core concepts stay stable.
";
        let doc_b = "\
---
title: Warehouse Guide
Created: 2027-05-05T12:00:00Z
UPDATED: 2027-06-06T12:00:00Z
Date: 2027-06-06
Crawled_At:
  - 2027-06-06T00:00:00Z
  - 2027-06-07T00:00:00Z
fetched_at: 2027-06-08T00:00:00Z
Last_Modified: Thu, 06 Jun 2027 00:00:00 GMT
tags:
  - data
---

# Warehouse Guide

Core concepts stay stable.
";
        let doc_c = "\
---
title: Warehouse Guide
tags:
  - data
---

# Warehouse Guide

Core concepts stay stable.
";

        let norm_a = normalize_markdown(doc_a, "fallback");
        let norm_b = normalize_markdown(doc_b, "fallback");
        let norm_c = normalize_markdown(doc_c, "fallback");

        assert_eq!(norm_a.content_hash.len(), 64);
        assert_eq!(norm_a.content_hash, norm_b.content_hash);
        assert_eq!(norm_a.content_hash, norm_c.content_hash);
        assert_eq!(norm_a.content_md, norm_c.content_md);

        // Documents with ONLY volatile frontmatter must match a document with no frontmatter at all.
        let volatile_only = "\
---
created: 2026-01-01
crawled_at: 2026-11-21T15:00:00Z
---

# Only Body
";
        let no_frontmatter = "# Only Body\n";
        let norm_vol = normalize_markdown(volatile_only, "fallback");
        let norm_plain = normalize_markdown(no_frontmatter, "fallback");
        assert_eq!(norm_vol.content_hash, norm_plain.content_hash);
        assert_eq!(norm_vol.content_md, norm_plain.content_md);
    }

    #[test]
    fn preserves_substantive_frontmatter_and_extracts_title() {
        let with_quoted_title = "\
---
title: \"Kimball's Dimensional Modeling\"
created_by: Alice
updated: 2026-11-21
---

# Heading That Should Lose To Frontmatter
";
        let norm = normalize_markdown(with_quoted_title, "ch01-kimball");
        assert_eq!(norm.title, "Kimball's Dimensional Modeling");
        assert!(norm.content_md.contains("created_by: Alice"));
        assert!(norm
            .content_md
            .contains("title: \"Kimball's Dimensional Modeling\""));
        assert!(!norm.content_md.contains("updated:"));

        let with_single_quoted_title = "---\ntitle: 'Single Quoted Title'\n---\n\nBody";
        assert_eq!(
            extract_title(with_single_quoted_title, "fallback"),
            "Single Quoted Title"
        );

        let with_h1_only = "\
```markdown
# Not A Real Heading
```

# Real **Markdown** `Title`

Body text.
";
        assert_eq!(
            extract_title(with_h1_only, "ch02-fallback"),
            "Real Markdown Title"
        );

        let no_title_or_h1 = "## Only Subheading\n\nSome content.";
        assert_eq!(extract_title(no_title_or_h1, "ch03-stem"), "ch03-stem");
    }

    #[test]
    fn normalizes_crlf_trailing_spaces_and_blank_lines() {
        let messy =
            "# Title   \r\n\r\nParagraph one.  \t \r\n\r\n\r\n\r\n\r\nParagraph two.\t\r\n\r\n\r\n";
        let cleaned = normalize_whitespace(messy);
        assert_eq!(cleaned, "# Title\n\nParagraph one.\n\n\nParagraph two.");

        let equivalent = "# Title\n\nParagraph one.\n\n\n\nParagraph two.\n";
        assert_eq!(
            normalize_markdown(messy, "x").content_hash,
            normalize_markdown(equivalent, "x").content_hash
        );
    }

    #[test]
    fn generates_unified_diff_capped_at_64kb() {
        assert_eq!(unified_diff("same\nline\n", "same\nline\n"), "");

        let small_diff = unified_diff("alpha\nbeta\n", "alpha\ngamma\n");
        assert!(small_diff.contains("--- previous"));
        assert!(small_diff.contains("+++ current"));
        assert!(small_diff.contains("-beta"));
        assert!(small_diff.contains("+gamma"));

        // Large ASCII + multi-byte UTF-8 diff exceeding 64 KB.
        let mut old_large = String::new();
        let mut new_large = String::new();
        for i in 0..2500 {
            old_large.push_str(&format!("line {i:04} — 日本語テキスト🦀 old value\n"));
            new_large.push_str(&format!("line {i:04} — 日本語テキスト🦀 new value\n"));
        }

        let capped = unified_diff(&old_large, &new_large);
        assert!(!capped.is_empty());
        assert!(
            capped.len() <= MAX_DIFF_BYTES,
            "diff byte length {} exceeded MAX_DIFF_BYTES {}",
            capped.len(),
            MAX_DIFF_BYTES
        );
        assert!(capped.contains("truncated"));
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
    }
}
