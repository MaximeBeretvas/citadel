use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use super::normalize::{
    append_heading_text, count_words, is_container_end, is_container_start, split_yaml_frontmatter,
    strip_bom,
};

/// A single heading-delimited section extracted from normalized Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionChunk {
    pub ordinal: usize,
    pub heading: String,
    pub heading_path: String,
    pub level: u8,
    pub content_md: String,
    pub word_count: usize,
}

/// Split normalized Markdown on H1–H3 headings while maintaining breadcrumb paths.
pub fn split_sections(content_md: &str) -> Vec<SectionChunk> {
    let body = match split_yaml_frontmatter(content_md) {
        Some(fm) => &content_md[fm.body_start..],
        None => strip_bom(content_md),
    };

    if body.trim().is_empty() {
        return Vec::new();
    }

    let mut sections: Vec<SectionChunk> = Vec::new();
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut current_heading: Option<(u8, String, String)> = None;
    let mut prev_body_start = 0usize;
    let mut container_depth = 0usize;
    let mut active_heading: Option<(u8, usize, String)> = None;

    for (event, range) in Parser::new(body).into_offset_iter() {
        match &event {
            Event::Start(tag) if is_container_start(tag) => {
                container_depth += 1;
            }
            Event::End(tag_end) if is_container_end(tag_end) => {
                container_depth = container_depth.saturating_sub(1);
            }
            Event::Start(Tag::Heading { level, .. }) if container_depth == 0 => {
                if let Some(lvl) = heading_level_u8(*level) {
                    active_heading = Some((lvl, range.start, String::new()));
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((lvl, h_start, raw_heading)) = active_heading.take() else {
                    continue;
                };
                let heading_text = raw_heading.trim().to_string();
                if heading_text.is_empty() {
                    continue;
                }

                flush_section(
                    &mut sections,
                    &mut current_heading,
                    &body[prev_body_start..h_start],
                );

                while stack
                    .last()
                    .is_some_and(|(existing_lvl, _)| *existing_lvl >= lvl)
                {
                    stack.pop();
                }
                stack.push((lvl, heading_text.clone()));

                let heading_path = stack
                    .iter()
                    .map(|(_, h)| h.as_str())
                    .collect::<Vec<_>>()
                    .join(" > ");

                current_heading = Some((lvl, heading_text, heading_path));
                prev_body_start = range.end;
            }
            _ => {
                if let Some((_, _, ref mut buf)) = active_heading {
                    append_heading_text(buf, &event);
                }
            }
        }
    }

    flush_section(
        &mut sections,
        &mut current_heading,
        &body[prev_body_start..],
    );

    sections
}

fn flush_section(
    sections: &mut Vec<SectionChunk>,
    current_heading: &mut Option<(u8, String, String)>,
    raw_slice: &str,
) {
    let slice = raw_slice.trim();
    match current_heading.take() {
        None => {
            if !slice.is_empty() {
                sections.push(SectionChunk {
                    ordinal: sections.len(),
                    heading: "(Intro)".to_string(),
                    heading_path: "(Intro)".to_string(),
                    level: 0,
                    word_count: count_words(slice),
                    content_md: slice.to_string(),
                });
            }
        }
        Some((level, heading, heading_path)) => {
            sections.push(SectionChunk {
                ordinal: sections.len(),
                heading,
                heading_path,
                level,
                word_count: count_words(slice),
                content_md: slice.to_string(),
            });
        }
    }
}

fn heading_level_u8(level: HeadingLevel) -> Option<u8> {
    match level {
        HeadingLevel::H1 => Some(1),
        HeadingLevel::H2 => Some(2),
        HeadingLevel::H3 => Some(3),
        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_h1_h2_h3_with_breadcrumbs() {
        let md = "\
---
title: Kimball Guide
---

Introductory context before any heading.

# Data Modeling

High-level overview.

## Kimball

Star schema basics.

### SCD Types

Slowly changing dimensions track history.

#### Type 2 Implementation

Effective dates and current flag.

## Inmon

Enterprise data warehouse approach.
";

        let sections = split_sections(md);
        assert_eq!(sections.len(), 5);

        assert_eq!(sections[0].ordinal, 0);
        assert_eq!(sections[0].level, 0);
        assert_eq!(sections[0].heading, "(Intro)");
        assert_eq!(sections[0].heading_path, "(Intro)");
        assert_eq!(
            sections[0].content_md,
            "Introductory context before any heading."
        );
        assert_eq!(sections[0].word_count, 5);

        assert_eq!(sections[1].ordinal, 1);
        assert_eq!(sections[1].level, 1);
        assert_eq!(sections[1].heading, "Data Modeling");
        assert_eq!(sections[1].heading_path, "Data Modeling");
        assert_eq!(sections[1].content_md, "High-level overview.");
        assert_eq!(sections[1].word_count, 2);

        assert_eq!(sections[2].ordinal, 2);
        assert_eq!(sections[2].level, 2);
        assert_eq!(sections[2].heading, "Kimball");
        assert_eq!(sections[2].heading_path, "Data Modeling > Kimball");
        assert_eq!(sections[2].content_md, "Star schema basics.");
        assert_eq!(sections[2].word_count, 3);

        assert_eq!(sections[3].ordinal, 3);
        assert_eq!(sections[3].level, 3);
        assert_eq!(sections[3].heading, "SCD Types");
        assert_eq!(
            sections[3].heading_path,
            "Data Modeling > Kimball > SCD Types"
        );
        assert!(sections[3]
            .content_md
            .contains("Slowly changing dimensions track history."));
        assert!(sections[3]
            .content_md
            .contains("#### Type 2 Implementation"));

        assert_eq!(sections[4].ordinal, 4);
        assert_eq!(sections[4].level, 2);
        assert_eq!(sections[4].heading, "Inmon");
        assert_eq!(sections[4].heading_path, "Data Modeling > Inmon");
        assert_eq!(
            sections[4].content_md,
            "Enterprise data warehouse approach."
        );
        assert_eq!(sections[4].word_count, 4);
    }

    #[test]
    fn ignores_hashes_inside_fenced_code_blocks() {
        let md = "\
# Setup

Run the setup script:

```python
# This is a Python comment, NOT an H1 heading
## Also a comment, NOT an H2 heading
def setup():
    return True
```

~~~bash
# Bash comment
echo \"done\"
~~~

Done with setup.
";

        let sections = split_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].ordinal, 0);
        assert_eq!(sections[0].level, 1);
        assert_eq!(sections[0].heading, "Setup");
        assert_eq!(sections[0].heading_path, "Setup");
        assert!(sections[0]
            .content_md
            .contains("# This is a Python comment"));
        assert!(sections[0].content_md.contains("# Bash comment"));
        assert!(sections[0].content_md.contains("Done with setup."));
    }

    #[test]
    fn handles_frontmatter_without_false_setext_heading_and_intro_only_docs() {
        let frontmatter_only_before_h1 = "\
---
title: Clean Page
tags: [a, b]
---

# Real Heading

Body paragraph.
";
        let sections = split_sections(frontmatter_only_before_h1);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].ordinal, 0);
        assert_eq!(sections[0].level, 1);
        assert_eq!(sections[0].heading, "Real Heading");
        assert_eq!(sections[0].content_md, "Body paragraph.");

        let no_headings = "---\ntitle: Note\n---\n\nJust a short note without headings.";
        let intro_sections = split_sections(no_headings);
        assert_eq!(intro_sections.len(), 1);
        assert_eq!(intro_sections[0].ordinal, 0);
        assert_eq!(intro_sections[0].level, 0);
        assert_eq!(intro_sections[0].heading, "(Intro)");
        assert_eq!(
            intro_sections[0].content_md,
            "Just a short note without headings."
        );

        assert!(split_sections("   \n\n  ").is_empty());
    }
}
