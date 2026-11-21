pub mod normalize;
pub mod sections;

pub use normalize::{
    count_words, extract_title, normalize_markdown, normalize_whitespace, sha256_hex,
    strip_volatile_frontmatter, unified_diff, NormalizedDocument, MAX_DIFF_BYTES,
};
pub use sections::{split_sections, SectionChunk};
