//! The Collected pages of one web Sync, and what each becomes.
//!
//! Every crawl of a web Sync hands its pages to [`CollectedPages`]. For each Collected page it
//! decides whether it becomes a Page, an Error page or a copy merged into another Page, or is
//! dropped, counted under the first reason it meets. It also sizes each later crawl, decides
//! whether the Sync is cut off, picks the missing-page policy and adds the budget warning. It does
//! no I/O: crawling, timing and persistence stay with the caller.

use std::collections::{HashMap, HashSet};

use spider::page::Page;
use spider_transformations::transformation::content::{
    transform_content, ReturnFormat, TransformConfig,
};
use url::Url;

use super::{canonical_dedup_key, canonicalize_page_url, resolve_seed_url, WebScope};
use crate::error::{Result, VaultError};
use crate::ingest::local::{MissingPagePolicy, PageIngestOutcome};
use crate::ingest::normalize::{count_words, normalize_markdown};
use crate::ingest::sections::split_sections;

/// Counts of the pages one web Sync's crawls delivered. When every crawl finishes, each page
/// received is counted once: as `results_stored`, `results_failed`, or under one of the eight drop
/// reasons (`unparsable_url` to `binary_file`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PageCounts {
    /// Every page any crawl delivered.
    pub pages_received: usize,
    /// Pages spider delivered that the receiving task missed because it fell behind.
    pub lost_by_listener: usize,
    pub later_crawls_started: usize,
    pub results_stored: usize,
    pub results_failed: usize,
    pub unparsable_url: usize,
    pub already_collected: usize,
    pub redirected_out_of_scope: usize,
    pub url_not_storable: usize,
    pub duplicate_result: usize,
    pub binary_file: usize,
    /// Markdown copies of a collected HTML page, stored as that page (issue #4).
    pub markdown_twin: usize,
    /// Pages whose `rel=canonical` names another collected page with the same path.
    pub canonical_duplicate: usize,
    /// Whether the Sync is a Cut-off Sync. Decided only when it has results to write.
    pub cut_off: bool,
}

/// What the next later crawl may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaterCrawl {
    /// An earlier crawl collected the seed, or started from it.
    Skip,
    /// The page budget is spent: start no more crawls.
    Stop,
    /// Crawl from the seed with this much of the page budget.
    Crawl(u32),
}

/// What a web Sync writes: one result per stored, failed or merged page, the policy for stored
/// pages it did not visit, and its warnings.
pub(super) struct Collected {
    pub outcomes: Vec<PageIngestOutcome>,
    pub missing_policy: MissingPagePolicy,
    pub warnings: Vec<String>,
}

/// The Collected pages of one web Sync. Hand it the first crawl's pages, then each later
/// crawl's, and [`finish`](Self::finish) it.
pub(super) struct CollectedPages<'a> {
    scope: &'a WebScope,
    max_pages: u32,
    pages: Vec<Page>,
    /// Dedup keys of the collected pages and of the seeds later crawls started from.
    visited_keys: HashSet<String>,
    /// Pages the first crawl delivered plus pages it lost: whether it used up spider's limit.
    first_crawl_delivered_or_lost: usize,
    /// The requested URLs of the first pages dropped as `redirected_out_of_scope`.
    redirect_examples: Vec<String>,
    counts: PageCounts,
}

impl<'a> CollectedPages<'a> {
    pub(super) fn new(scope: &'a WebScope, max_pages: u32) -> Self {
        Self {
            scope,
            max_pages,
            pages: Vec::new(),
            visited_keys: HashSet::new(),
            first_crawl_delivered_or_lost: 0,
            redirect_examples: Vec::new(),
            counts: PageCounts::default(),
        }
    }

    /// Take the first crawl's pages and the number it lost. Every page is kept: an unparsable URL
    /// or a second copy of a URI is judged in [`finish`](Self::finish).
    pub(super) fn add_first_crawl(&mut self, pages: Vec<Page>, lost: usize) {
        self.first_crawl_delivered_or_lost = pages.len() + lost;
        self.counts.pages_received += pages.len();
        self.counts.lost_by_listener += lost;
        for page in pages {
            if let Some(key) = page_dedup_key(&page) {
                self.visited_keys.insert(key);
            }
            self.pages.push(page);
        }
    }

    /// Whether to crawl from `seed` next, and with how much budget. `Crawl` counts the crawl as
    /// started. The budget counts documents, so a Markdown twin pair takes one page of it.
    pub(super) fn next_later_crawl(&mut self, seed: &Url) -> LaterCrawl {
        if self.visited_keys.contains(&canonical_dedup_key(seed)) {
            return LaterCrawl::Skip;
        }
        let used_budget = count_scoped_documents(&self.pages, self.scope);
        if used_budget >= self.max_pages as usize {
            return LaterCrawl::Stop;
        }
        self.counts.later_crawls_started += 1;
        LaterCrawl::Crawl(self.max_pages - used_budget as u32)
    }

    /// Take the pages of the later crawl started from `seed`, and the number it lost. A page whose
    /// URL cannot be parsed, or that an earlier crawl collected, is dropped and counted here.
    pub(super) fn add_later_crawl(&mut self, seed: &Url, pages: Vec<Page>, lost: usize) {
        self.counts.pages_received += pages.len();
        self.counts.lost_by_listener += lost;
        for page in pages {
            let Some(key) = page_dedup_key(&page) else {
                self.counts.unparsable_url += 1;
                continue;
            };
            if self.visited_keys.insert(key) {
                self.pages.push(page);
            } else {
                self.counts.already_collected += 1;
            }
        }
        self.visited_keys.insert(canonical_dedup_key(seed));
    }

    /// The counts so far, for the summary line of a Sync whose crawl failed.
    pub(super) fn counts(&self) -> &PageCounts {
        &self.counts
    }

    /// Decide what every collected page becomes, whether the Sync is cut off, and what happens to
    /// stored pages it did not visit (`prune` deletes them). Fails when no page was stored and
    /// every result is a transport failure, or there is no result at all. The counts come back
    /// either way.
    pub(super) fn finish(mut self, prune: bool) -> (Result<Collected>, PageCounts) {
        let max_pages = self.max_pages as usize;
        let collected_storable_urls = count_scoped_urls(&self.pages, self.scope);
        let pages = std::mem::take(&mut self.pages);
        let (outcomes, ok_count, transport_failures) = build_page_outcomes(
            pages,
            self.scope,
            &mut self.counts,
            &mut self.redirect_examples,
        );

        let results = result_count(&outcomes);
        if ok_count == 0 && (results == 0 || transport_failures == results) {
            let reason = outcomes
                .iter()
                .find_map(|o| match o {
                    PageIngestOutcome::Failed { error_message, .. } => Some(error_message.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    format!("no reachable pages crawled from '{}'", self.scope.origin)
                });
            return (Err(VaultError::Other(reason)), self.counts);
        }

        self.counts.cut_off = is_cut_off(
            self.first_crawl_delivered_or_lost,
            collected_storable_urls,
            outcomes.len(),
            max_pages,
        );
        let missing_policy = if prune {
            MissingPagePolicy::Prune
        } else if self.counts.cut_off {
            MissingPagePolicy::Preserve
        } else {
            MissingPagePolicy::MarkStale
        };
        let warnings = budget_warning(&self.counts, &self.redirect_examples, max_pages)
            .into_iter()
            .collect();
        let collected = Collected {
            outcomes,
            missing_policy,
            warnings,
        };
        (Ok(collected), self.counts)
    }
}

/// Distinct storable URLs among the collected pages. The cut-off rule uses this count: spider's
/// limit counts every fetch, Markdown twins included, so it must not be lowered by pairing.
fn count_scoped_urls(pages: &[Page], scope: &WebScope) -> usize {
    scoped_urls(pages, scope).len()
}

/// Distinct documents among the storable collected pages: a Markdown URL whose HTML page was also
/// collected (`X` and `X.md`) counts once. Sizes the budget of later crawls only, so twins do not
/// take the place of other documents; never feed it to `is_cut_off`.
fn count_scoped_documents(pages: &[Page], scope: &WebScope) -> usize {
    let urls = scoped_urls(pages, scope);
    let twins: HashSet<String> = urls
        .values()
        .filter(|url| !url.path().ends_with(".md"))
        .flat_map(markdown_twin_keys)
        .filter(|key| urls.contains_key(key))
        .collect();
    urls.len() - twins.len()
}

/// The storable collected URLs, by dedup key.
fn scoped_urls(pages: &[Page], scope: &WebScope) -> HashMap<String, Url> {
    pages
        .iter()
        .filter_map(|page| canonicalize_page_url(page.get_url()))
        .filter(|url| scope.should_store(url))
        .map(|url| (canonical_dedup_key(&url), url))
        .collect()
}

/// Turn collected pages into results, counting each page that gets none in `counts` under the first
/// reason it meets. Copies of one document (see [`pair_documents`]) become one result at the
/// document's URI plus a `Retired` result per copy.
fn build_page_outcomes(
    pages: Vec<Page>,
    scope: &WebScope,
    counts: &mut PageCounts,
    redirect_examples: &mut Vec<String>,
) -> (Vec<PageIngestOutcome>, usize, usize) {
    let superseded = pair_documents(&pages, scope);
    let twin_of: HashMap<usize, usize> = superseded
        .iter()
        .filter_map(|(&twin, kept)| match kept {
            Kept::MarkdownTwinOf(html) => Some((*html, twin)),
            Kept::CanonicalOf(_) => None,
        })
        .collect();
    let mut outcomes = Vec::new();
    let mut seen_uris = HashSet::new();
    let mut ok_count = 0usize;
    let mut transport_failures = 0usize;

    for (index, page) in pages.iter().enumerate() {
        let Some(canonical_url) = canonicalize_page_url(page.get_url()) else {
            counts.unparsable_url += 1;
            continue;
        };

        if redirected_out_of_scope(page, scope) {
            counts.redirected_out_of_scope += 1;
            if redirect_examples.len() < MAX_REDIRECT_EXAMPLES {
                redirect_examples.push(page.get_url().to_string());
            }
            continue;
        }

        if !scope.should_store(&canonical_url) {
            counts.url_not_storable += 1;
            continue;
        }

        let uri = canonical_url.to_string();
        if !seen_uris.insert(uri.clone()) {
            counts.duplicate_result += 1;
            continue;
        }

        if let Some(kept) = superseded.get(&index) {
            match kept {
                Kept::MarkdownTwinOf(_) => counts.markdown_twin += 1,
                Kept::CanonicalOf(_) => counts.canonical_duplicate += 1,
            }
            tracing::debug!(
                uri = %uri,
                by = pages[kept.index()].get_url(),
                "page is a copy of another collected page"
            );
            outcomes.push(PageIngestOutcome::Retired { uri });
            continue;
        }

        let status_u16 = page.status_code.as_u16();
        if page_failed(page) {
            let is_transport = status_u16 == 0 || status_u16 >= 520 || page.error_status.is_some();
            if is_transport {
                transport_failures += 1;
            }
            let error_message = format_page_error(page, status_u16);
            outcomes.push(PageIngestOutcome::Failed {
                uri,
                http_status: (status_u16 > 0).then_some(i64::from(status_u16)),
                error_message,
                insert_if_missing: true,
            });
            counts.results_failed += 1;
            continue;
        }

        if page.binary_file {
            counts.binary_file += 1;
            continue;
        }

        // A cleanly fetched Markdown twin replaces the HTML conversion; the HTML page keeps its
        // URI, its `<title>` as the fallback title and its HTTP validators.
        let twin = twin_of
            .get(&index)
            .map(|&twin| &pages[twin])
            .filter(|twin| fetched_ok(twin));
        let (raw_md, html_title) = match twin {
            Some(twin) => (twin.get_html(), extract_html_title(&page.get_html_cow())),
            None => extract_page_markdown_and_title(page, &canonical_url),
        };
        let fallback_title = html_title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| fallback_title_from_url(&canonical_url));

        let doc = normalize_markdown(&raw_md, &fallback_title);
        let chunks = split_sections(&doc.content_md);
        let (etag, last_modified) = extract_http_validators(page);

        ok_count += 1;
        counts.results_stored += 1;
        outcomes.push(PageIngestOutcome::Ready {
            uri,
            doc,
            chunks,
            etag,
            last_modified,
            http_status: Some(i64::from(status_u16)),
        });
    }

    (outcomes, ok_count, transport_failures)
}

/// Whether a page was redirected to a URL the crawl would not follow.
fn redirected_out_of_scope(page: &Page, scope: &WebScope) -> bool {
    page.final_redirect_destination
        .as_deref()
        .and_then(canonicalize_page_url)
        .is_some_and(|dest| !scope.should_crawl(&dest))
}

/// A page whose fetch failed: an HTTP error status, no status at all, or a transport error.
fn page_failed(page: &Page) -> bool {
    let status = page.status_code.as_u16();
    status >= 400 || status == 0 || page.error_status.is_some()
}

/// A page that fetched cleanly with text content.
fn fetched_ok(page: &Page) -> bool {
    !page_failed(page) && !page.binary_file
}

/// The page whose result a superseded page's document is stored under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kept {
    /// The superseded page is the Markdown twin of the HTML page at this index.
    MarkdownTwinOf(usize),
    /// The superseded page is a canonical duplicate of the page at this index.
    CanonicalOf(usize),
}

impl Kept {
    fn index(self) -> usize {
        match self {
            Self::MarkdownTwinOf(i) | Self::CanonicalOf(i) => i,
        }
    }
}

/// Find the copies of one document among the collected pages, as a map from each superseded
/// page's index to the page its document is stored under:
/// - **Canonical duplicate:** an HTML page whose `<link rel="canonical">` names another
///   collected, cleanly fetched page with the same path (only the query string or a trailing
///   slash differs), e.g. `/guide?version=2` -> `/guide`. Requiring the same path keeps a site
///   that points every canonical at its home page from collapsing into one page.
/// - **Markdown twin:** the Markdown copy of a cleanly fetched HTML page `X`: the target of its
///   `<link rel="alternate" type="text/markdown">`, else `X.md`, else `X/index.md`, when that URL
///   was collected and is Markdown. A twin whose HTML page failed is not paired, so it is
///   handled like any other page.
fn pair_documents(pages: &[Page], scope: &WebScope) -> HashMap<usize, Kept> {
    // Only the first page per exact URI takes part, as only it gets a result in
    // `build_page_outcomes`. Lookups try the exact URI before the dedup key, so the variant a
    // canonical tag or twin link names wins over whichever of `/p` and `/p/` arrived first.
    let mut index_by_uri: HashMap<String, usize> = HashMap::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    let mut urls: Vec<Option<Url>> = Vec::with_capacity(pages.len());
    for (index, page) in pages.iter().enumerate() {
        let url = canonicalize_page_url(page.get_url())
            .filter(|url| scope.should_store(url) && !redirected_out_of_scope(page, scope))
            .filter(|url| !index_by_uri.contains_key(url.as_str()));
        if let Some(url) = &url {
            index_by_uri.insert(url.to_string(), index);
            index_by_key
                .entry(canonical_dedup_key(url))
                .or_insert(index);
        }
        urls.push(url);
    }
    let lookup = |exact: &str, key: &str| {
        index_by_uri
            .get(exact)
            .or_else(|| index_by_key.get(key))
            .copied()
    };

    let mut superseded: HashMap<usize, Kept> = HashMap::new();
    let html_pages: Vec<(usize, &Url, HeadLinks)> = pages
        .iter()
        .zip(&urls)
        .enumerate()
        .filter_map(|(index, (page, url))| {
            let url = url.as_ref()?;
            (fetched_ok(page) && !is_markdown_response(page, url))
                .then(|| (index, url, head_links(&page.get_html_cow(), url)))
        })
        .collect();

    for (index, url, links) in &html_pages {
        let Some(canonical) = &links.canonical else {
            continue;
        };
        let Some(kept) = lookup(canonical.as_str(), &canonical_dedup_key(canonical)) else {
            continue;
        };
        let same_path = canonical.path().trim_end_matches('/') == url.path().trim_end_matches('/');
        if kept != *index && same_path && fetched_ok(&pages[kept]) {
            superseded.insert(*index, Kept::CanonicalOf(kept));
        }
    }
    // A page kept for a duplicate must not itself be superseded (a canonical chain or cycle).
    let chained: Vec<usize> = superseded
        .iter()
        .filter(|(_, kept)| superseded.contains_key(&kept.index()))
        .map(|(index, _)| *index)
        .collect();
    for index in chained {
        superseded.remove(&index);
    }

    for (index, url, links) in &html_pages {
        if superseded.contains_key(index) {
            continue;
        }
        let candidates = links
            .markdown_alternate
            .iter()
            .map(|alt| (alt.to_string(), canonical_dedup_key(alt)))
            .chain(markdown_twin_keys(url).into_iter().map(|k| (k.clone(), k)));
        let twin = candidates
            .filter_map(|(exact, key)| lookup(&exact, &key))
            .find(|&twin| {
                twin != *index
                    && !superseded.contains_key(&twin)
                    && urls[twin]
                        .as_ref()
                        .is_some_and(|twin_url| is_markdown_response(&pages[twin], twin_url))
            });
        if let Some(twin) = twin {
            superseded.insert(twin, Kept::MarkdownTwinOf(*index));
        }
    }

    superseded
}

/// The `<link>` targets in a page that name other copies of it.
#[derive(Debug, Default, PartialEq, Eq)]
struct HeadLinks {
    canonical: Option<Url>,
    markdown_alternate: Option<Url>,
}

/// Read the first `rel="canonical"` and `rel="alternate" type="text/markdown"` links of an HTML
/// page, resolved against `base`. Tolerates minified HTML (unquoted attributes). Parsing stops at
/// `<body>`, where head links end.
fn head_links(html: &str, base: &Url) -> HeadLinks {
    const BODY_REACHED: &str = "body reached";
    let mut found = HeadLinks::default();
    let mut rewriter = lol_html::HtmlRewriter::new(
        lol_html::Settings {
            element_content_handlers: vec![
                lol_html::element!("body", |_| Err(BODY_REACHED.into())),
                lol_html::element!("link[rel][href]", |el| {
                    let rel = el.get_attribute("rel").unwrap_or_default();
                    let has_rel = |name: &str| {
                        rel.split_ascii_whitespace()
                            .any(|r| r.eq_ignore_ascii_case(name))
                    };
                    let href = el.get_attribute("href").unwrap_or_default();
                    if has_rel("canonical") && found.canonical.is_none() {
                        found.canonical = resolve_seed_url(base, &href);
                    } else if has_rel("alternate") && found.markdown_alternate.is_none() {
                        let kind = el.get_attribute("type").unwrap_or_default();
                        let kind = kind.to_ascii_lowercase();
                        if kind.starts_with("text/markdown") || kind.starts_with("text/x-markdown")
                        {
                            found.markdown_alternate = resolve_seed_url(base, &href);
                        }
                    }
                    Ok(())
                }),
            ],
            ..lol_html::Settings::new()
        },
        |_: &[u8]| {},
    );
    let parsed = rewriter
        .write(html.as_bytes())
        .and_then(|()| rewriter.end());
    match parsed {
        Err(lol_html::errors::RewritingError::ContentHandlerError(err))
            if err.to_string() == BODY_REACHED => {}
        Err(err) => tracing::debug!(url = %base, error = %err, "could not read link tags of page"),
        Ok(()) => {}
    }
    found
}

/// Dedup keys of the URLs a site may serve the Markdown copy of HTML page `url` at: `X.md` and
/// `X/index.md` (for `X` and `X/`), without the query string.
fn markdown_twin_keys(url: &Url) -> Vec<String> {
    let mut base = url.clone();
    base.set_query(None);
    base.set_fragment(None);
    let path = base.path().trim_end_matches('/').to_string();
    let mut keys = Vec::with_capacity(2);
    if !path.is_empty() {
        let mut twin = base.clone();
        twin.set_path(&format!("{path}.md"));
        keys.push(canonical_dedup_key(&twin));
    }
    let mut index = base;
    index.set_path(&format!("{path}/index.md"));
    keys.push(canonical_dedup_key(&index));
    keys
}

fn format_page_error(page: &Page, status_u16: u16) -> String {
    let reason = page.status_code.canonical_reason().unwrap_or("Error");
    match page.error_status.as_deref() {
        Some(err) if !err.trim().is_empty() => {
            if status_u16 > 0 {
                format!("HTTP {status_u16} ({reason}): {err}")
            } else {
                err.to_string()
            }
        }
        _ => format!("HTTP {status_u16} {reason}"),
    }
}

fn extract_http_validators(page: &Page) -> (Option<String>, Option<String>) {
    let Some(headers) = page.headers.as_ref() else {
        return (None, None);
    };
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let last_modified = headers
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    (etag, last_modified)
}

fn extract_page_markdown_and_title(page: &Page, url: &Url) -> (String, Option<String>) {
    let raw_content = page.get_html();
    if page.content_is_markdown || is_markdown_response(page, url) {
        return (raw_content, None);
    }

    let html_title = extract_html_title(&raw_content);
    let ignore_tags = Some(vec![
        "nav".to_string(),
        "footer".to_string(),
        "script".to_string(),
        "style".to_string(),
        "noscript".to_string(),
        "title".to_string(),
    ]);

    let readability_cfg = TransformConfig {
        readability: true,
        return_format: ReturnFormat::Markdown,
        filter_images: false,
        clean_html: false,
        filter_svg: true,
        main_content: false,
    };
    let readability_md = transform_content(page, &readability_cfg, &None, &None, &ignore_tags);

    let fallback_cfg = TransformConfig {
        readability: false,
        ..readability_cfg
    };
    let fallback_md = transform_content(page, &fallback_cfg, &None, &None, &ignore_tags);

    let chosen = if should_prefer_fallback(&readability_md, &fallback_md) {
        fallback_md
    } else {
        readability_md
    };

    (chosen, html_title)
}

fn should_prefer_fallback(readability_md: &str, fallback_md: &str) -> bool {
    if fallback_md.trim().is_empty() {
        return false;
    }
    if readability_md.trim().is_empty() {
        return true;
    }
    let read_words = count_words(readability_md);
    let fall_words = count_words(fallback_md);
    if !readability_md.contains("# ") && fallback_md.contains("# ") {
        return true;
    }
    fall_words > 10 && read_words.saturating_mul(4) < fall_words
}

fn is_markdown_response(page: &Page, url: &Url) -> bool {
    if let Some(headers) = page.headers.as_ref() {
        if let Some(ct) = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_ascii_lowercase)
        {
            if ct.contains("text/markdown") || ct.contains("text/x-markdown") {
                return true;
            }
        }
    }
    let path_lower = url.path().to_ascii_lowercase();
    path_lower.ends_with(".md") || path_lower.ends_with(".markdown")
}

/// Whether a Sync stopped because it used up its page budget, so pages it did not visit must be
/// kept. Any one condition is enough:
/// 1. the first crawl delivered or lost `max_pages` pages: spider's limit ran out (a single
///    crawler cannot fetch a page twice);
/// 2. the distinct storable URLs among the collected pages reach `max_pages`;
/// 3. the results reach `max_pages`: stored, failed and merged copies (`Retired`) all count.
fn is_cut_off(
    first_crawl_delivered_or_lost: usize,
    collected_storable_urls: usize,
    outcomes: usize,
    max_pages: usize,
) -> bool {
    first_crawl_delivered_or_lost >= max_pages
        || collected_storable_urls >= max_pages
        || outcomes >= max_pages
}

/// A cut-off Sync warns about redirects only when at least this many pages redirected outside
/// the Source...
const MIN_REDIRECTS_FOR_WARNING: usize = 5;
/// ...and they are more than this share (in percent) of the pages received.
const REDIRECT_SHARE_PERCENT: usize = 10;
/// How many redirected URLs the warning names.
const MAX_REDIRECT_EXAMPLES: usize = 3;

/// The warning for a cut-off Sync that spent much of its page budget on pages that redirected
/// outside the Source, or `None`.
fn budget_warning(
    counts: &PageCounts,
    redirect_examples: &[String],
    max_pages: usize,
) -> Option<String> {
    let redirected = counts.redirected_out_of_scope;
    let received = counts.pages_received;
    if !counts.cut_off
        || redirected < MIN_REDIRECTS_FOR_WARNING
        || redirected * 100 <= received * REDIRECT_SHARE_PERCENT
    {
        return None;
    }
    let examples = redirect_examples.join(", ");
    Some(format!(
        "{redirected} of {received} pages received redirected outside this Source and were not \
         stored, for example {examples}. They count against max_pages ({max_pages}), so other \
         pages were not reached. Raise max_pages, or exclude that section if you do not need it."
    ))
}

/// Results that stand for a page of their own (stored or failed), leaving out superseded copies.
fn result_count(outcomes: &[PageIngestOutcome]) -> usize {
    outcomes
        .iter()
        .filter(|o| !matches!(o, PageIngestOutcome::Retired { .. }))
        .count()
}

fn page_dedup_key(page: &Page) -> Option<String> {
    canonicalize_page_url(page.get_url()).map(|u| canonical_dedup_key(&u))
}

fn fallback_title_from_url(url: &Url) -> String {
    url.path_segments()
        .and_then(|segments| segments.rev().find(|s| !s.is_empty()))
        .map(|seg| seg.replace(['-', '_'], " "))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| url.as_str().to_string())
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start_tag = lower.find("<title")?;
    let after_open = &lower[start_tag + 6..];
    let gt_rel = after_open.find('>')?;
    let content_start = start_tag + 6 + gt_rel + 1;
    let end_rel = lower[content_start..].find("</title>")?;
    let raw = html[content_start..content_start + end_rel].trim();
    if raw.is_empty() {
        return None;
    }
    let decoded = decode_basic_html_entities(raw);
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

fn decode_basic_html_entities(raw: &str) -> String {
    raw.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{Collected, CollectedPages, LaterCrawl, PageCounts};
    use crate::error::Result;
    use crate::ingest::local::{MissingPagePolicy, PageIngestOutcome};
    use crate::ingest::web::{Page, Url, WebScope};

    fn scope() -> WebScope {
        let origin = Url::parse("https://docs.example/docs/").expect("origin");
        WebScope::new(&origin, &[], &[]).expect("scope")
    }

    fn page(url: &str) -> Page {
        let mut page = Page::default();
        page.set_url(url.to_string());
        page.status_code = StatusCode::OK;
        page
    }

    /// A page served with `content_type` and `body`.
    fn typed_page(url: &str, content_type: &str, body: &str) -> Page {
        let mut page = page(url);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            content_type.parse().expect("content type"),
        );
        page.headers = Some(headers);
        page.set_html_bytes(Some(body.as_bytes().to_vec()));
        page
    }

    /// A page whose request was redirected outside the Source.
    fn redirected(url: &str) -> Page {
        let mut page = page(url);
        page.final_redirect_destination = Some("https://elsewhere.example/x".to_string());
        page
    }

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("url")
    }

    /// Each result as (kind, URI), in order.
    fn results(collected: &Collected) -> Vec<(&'static str, &str)> {
        collected
            .outcomes
            .iter()
            .map(|outcome| match outcome {
                PageIngestOutcome::Ready { uri, .. } => ("ready", uri.as_str()),
                PageIngestOutcome::Failed { uri, .. } => ("failed", uri.as_str()),
                PageIngestOutcome::Retired { uri } => ("retired", uri.as_str()),
            })
            .collect()
    }

    /// Collect one first crawl and finish.
    fn first_crawl(
        pages: Vec<Page>,
        lost: usize,
        max_pages: u32,
        prune: bool,
    ) -> (Result<Collected>, PageCounts) {
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, max_pages);
        collected.add_first_crawl(pages, lost);
        collected.finish(prune)
    }

    #[test]
    fn counts_each_dropped_page_under_its_first_reason() {
        let scope = scope();
        let mut redirected = page("https://docs.example/docs/moved");
        redirected.final_redirect_destination = Some("https://elsewhere.example/x".to_string());
        let mut binary = page("https://docs.example/docs/file");
        binary.binary_file = true;
        let mut missing = page("https://docs.example/docs/missing");
        missing.status_code = StatusCode::NOT_FOUND;

        let mut collected = CollectedPages::new(&scope, 100);
        collected.add_first_crawl(
            vec![
                page("ftp://docs.example/docs/unparsable"),
                redirected,
                page("https://docs.example/blog/outside"),
                page("https://docs.example/docs/stored"),
                page("https://docs.example/docs/stored"),
                binary,
                missing,
            ],
            0,
        );
        let (result, counts) = collected.finish(false);

        assert_eq!(result.expect("one page is stored").outcomes.len(), 2);
        assert_eq!(
            counts,
            PageCounts {
                pages_received: 7,
                unparsable_url: 1,
                redirected_out_of_scope: 1,
                url_not_storable: 1,
                duplicate_result: 1,
                binary_file: 1,
                results_stored: 1,
                results_failed: 1,
                ..PageCounts::default()
            }
        );
    }

    #[test]
    fn later_crawl_drops_duplicates_and_unparsable_urls_at_intake() {
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 100);
        collected.add_first_crawl(
            vec![
                page("https://docs.example/docs/"),
                page("https://docs.example/docs/a"),
            ],
            0,
        );
        let seed = url("https://docs.example/docs/h1");
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Crawl(98));
        collected.add_later_crawl(
            &seed,
            vec![
                page("https://docs.example/docs/h1"),
                page("https://docs.example/docs/a"),
                page("https://docs.example/docs/a/"),
                page("ftp://docs.example/docs/x"),
            ],
            1,
        );
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Skip);

        let (result, counts) = collected.finish(false);
        assert_eq!(result.expect("pages stored").outcomes.len(), 3);
        assert_eq!(
            counts,
            PageCounts {
                pages_received: 6,
                lost_by_listener: 1,
                later_crawls_started: 1,
                already_collected: 2,
                unparsable_url: 1,
                results_stored: 3,
                ..PageCounts::default()
            }
        );
    }

    #[test]
    fn next_later_crawl_skips_a_collected_seed_and_stops_when_the_budget_is_spent() {
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 2);
        collected.add_first_crawl(vec![page("https://docs.example/docs/a")], 0);

        assert_eq!(
            collected.next_later_crawl(&url("https://docs.example/docs/a/")),
            LaterCrawl::Skip,
            "same dedup key as a collected page"
        );
        let seed = url("https://docs.example/docs/b");
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Crawl(1));
        collected.add_later_crawl(&seed, vec![page("https://docs.example/docs/b")], 0);
        assert_eq!(
            collected.next_later_crawl(&url("https://docs.example/docs/c")),
            LaterCrawl::Stop
        );

        let (_, counts) = collected.finish(false);
        assert_eq!(counts.later_crawls_started, 1);
    }

    #[test]
    fn later_crawl_budget_counts_a_markdown_twin_pair_once() {
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 6);
        collected.add_first_crawl(
            vec![
                typed_page("https://docs.example/docs/guide", "text/html", ""),
                typed_page("https://docs.example/docs/guide.md", "text/markdown", ""),
                typed_page("https://docs.example/docs/orphan.md", "text/markdown", ""),
                typed_page("https://docs.example/docs/", "text/html", ""),
                typed_page("https://docs.example/docs/index.md", "text/markdown", ""),
            ],
            0,
        );
        // Five storable URLs, three documents: guide + guide.md, orphan.md, docs/ + index.md.
        assert_eq!(
            collected.next_later_crawl(&url("https://docs.example/docs/next")),
            LaterCrawl::Crawl(3)
        );
    }

    #[test]
    fn every_received_page_lands_in_exactly_one_count() {
        let html = r#"<html><head><link rel="canonical" href="/docs/guide"></head><body><h1>Guide</h1></body></html>"#;
        let mut binary = page("https://docs.example/docs/file");
        binary.binary_file = true;
        let mut missing = page("https://docs.example/docs/missing");
        missing.status_code = StatusCode::NOT_FOUND;

        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 100);
        collected.add_first_crawl(
            vec![
                page("ftp://docs.example/docs/unparsable"),
                redirected("https://docs.example/docs/moved"),
                page("https://docs.example/blog/outside"),
                page("https://docs.example/docs/a"),
                page("https://docs.example/docs/a"),
                binary,
                missing,
                typed_page("https://docs.example/docs/guide", "text/html", html),
                typed_page(
                    "https://docs.example/docs/guide.md",
                    "text/markdown",
                    "# Guide\n\nMd.\n",
                ),
                typed_page(
                    "https://docs.example/docs/guide?version=2",
                    "text/html",
                    html,
                ),
            ],
            0,
        );
        // Six documents so far: the redirected page's requested URL counts against the budget,
        // and guide.md pairs with guide.
        let seed = url("https://docs.example/docs/h1");
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Crawl(94));
        collected.add_later_crawl(
            &seed,
            vec![
                page("https://docs.example/docs/h1"),
                page("https://docs.example/docs/a"),
                page("ftp://docs.example/docs/y"),
            ],
            0,
        );

        let (_, counts) = collected.finish(false);
        assert_eq!(
            counts,
            PageCounts {
                pages_received: 13,
                later_crawls_started: 1,
                results_stored: 3,
                results_failed: 1,
                unparsable_url: 2,
                already_collected: 1,
                redirected_out_of_scope: 1,
                url_not_storable: 1,
                duplicate_result: 1,
                binary_file: 1,
                markdown_twin: 1,
                canonical_duplicate: 1,
                ..PageCounts::default()
            }
        );
        let landed = counts.results_stored
            + counts.results_failed
            + counts.unparsable_url
            + counts.already_collected
            + counts.redirected_out_of_scope
            + counts.url_not_storable
            + counts.duplicate_result
            + counts.binary_file
            + counts.markdown_twin
            + counts.canonical_duplicate;
        assert_eq!(landed, 13, "13 pages were handed over");
    }

    #[test]
    fn reads_link_tags_from_minified_html() {
        let base = url("https://docs.example/docs/guide?version=2");
        let links = super::head_links(
            "<html><head><link rel=canonical href=/docs/guide><link data-rh=true \
             rel=alternate type=text/markdown href=guide.md></head><body></body></html>",
            &base,
        );
        assert_eq!(
            links.canonical.map(|u| u.to_string()).as_deref(),
            Some("https://docs.example/docs/guide")
        );
        assert_eq!(
            links.markdown_alternate.map(|u| u.to_string()).as_deref(),
            Some("https://docs.example/docs/guide.md")
        );
    }

    #[test]
    fn canonical_to_another_path_does_not_collapse_pages() {
        let html = r#"<html><head><link rel="canonical" href="https://docs.example/docs/"></head><body><h1>Page</h1><p>Text.</p></body></html>"#;
        let (result, counts) = first_crawl(
            vec![
                typed_page("https://docs.example/docs/", "text/html", html),
                typed_page("https://docs.example/docs/a", "text/html", html),
                typed_page("https://docs.example/docs/b", "text/html", html),
            ],
            0,
            100,
            false,
        );
        assert_eq!(result.expect("stored").outcomes.len(), 3);
        assert_eq!(counts.results_stored, 3);
        assert_eq!(counts.canonical_duplicate, 0);
    }

    #[test]
    fn twins_and_canonical_duplicates_are_counted_and_retired() {
        let html = r#"<html><head><link rel="canonical" href="/docs/guide"></head><body><h1>Guide</h1><p>Html text.</p></body></html>"#;
        let (result, counts) = first_crawl(
            vec![
                typed_page(
                    "https://docs.example/docs/guide.md",
                    "text/markdown",
                    "# Guide\n\nMd text.\n",
                ),
                typed_page("https://docs.example/docs/guide", "text/html", html),
                typed_page(
                    "https://docs.example/docs/guide?version=2",
                    "text/html",
                    html,
                ),
                typed_page(
                    "https://docs.example/docs/index.md",
                    "text/markdown",
                    "# Home\n\nMd home.\n",
                ),
                typed_page(
                    "https://docs.example/docs/",
                    "text/html",
                    "<html><body><h1>Home</h1></body></html>",
                ),
            ],
            0,
            100,
            false,
        );
        let collected = result.expect("stored");

        assert_eq!(
            results(&collected),
            vec![
                ("retired", "https://docs.example/docs/guide.md"),
                ("ready", "https://docs.example/docs/guide"),
                ("retired", "https://docs.example/docs/guide?version=2"),
                ("retired", "https://docs.example/docs/index.md"),
                ("ready", "https://docs.example/docs/"),
            ]
        );
        assert_eq!(counts.results_stored, 2);
        assert_eq!(counts.markdown_twin, 2);
        assert_eq!(counts.canonical_duplicate, 1);
        for outcome in &collected.outcomes {
            if let PageIngestOutcome::Ready { uri, doc, .. } = outcome {
                assert!(doc.content_md.contains("Md "), "{uri}: {}", doc.content_md);
            }
        }
    }

    #[test]
    fn a_failed_first_copy_of_a_page_claims_no_twin() {
        let mut failed = typed_page("https://docs.example/docs/guide", "text/html", "");
        failed.status_code = StatusCode::NOT_FOUND;
        let (result, counts) = first_crawl(
            vec![
                failed,
                typed_page(
                    "https://docs.example/docs/guide",
                    "text/html",
                    "<h1>Guide</h1>",
                ),
                typed_page(
                    "https://docs.example/docs/guide.md",
                    "text/markdown",
                    "# Guide\n\nMd.\n",
                ),
            ],
            0,
            100,
            false,
        );
        assert_eq!(
            results(&result.expect("stored")),
            vec![
                ("failed", "https://docs.example/docs/guide"),
                ("ready", "https://docs.example/docs/guide.md"),
            ],
            "the .md is stored on its own"
        );
        assert_eq!(counts.markdown_twin, 0, "{counts:?}");
    }

    #[test]
    fn canonical_names_the_exact_trailing_slash_variant() {
        let html = r#"<html><head><link rel="canonical" href="/docs/p/"></head><body><h1>P</h1></body></html>"#;
        let (result, counts) = first_crawl(
            vec![
                typed_page("https://docs.example/docs/p", "text/html", html),
                typed_page("https://docs.example/docs/p/", "text/html", html),
            ],
            0,
            100,
            false,
        );
        assert_eq!(
            results(&result.expect("stored")),
            vec![
                ("retired", "https://docs.example/docs/p"),
                ("ready", "https://docs.example/docs/p/"),
            ]
        );
        assert_eq!(counts.canonical_duplicate, 1);
    }

    #[test]
    fn first_crawl_at_limit_is_cut_off() {
        // The dlthub Sync of 2026-11-23: spider's limit ran out, though pages were lost.
        let (result, counts) = first_crawl(
            vec![
                page("https://docs.example/docs/a"),
                page("https://docs.example/docs/b"),
            ],
            1,
            3,
            false,
        );
        assert!(counts.cut_off, "{counts:?}");
        assert_eq!(
            result.expect("stored").missing_policy,
            MissingPagePolicy::Preserve
        );
    }

    #[test]
    fn collected_urls_at_limit_is_cut_off() {
        // Pages that redirect out of the Source get no result but still use up the budget.
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 3);
        collected.add_first_crawl(vec![page("https://docs.example/docs/a")], 0);
        let seed = url("https://docs.example/docs/r1");
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Crawl(2));
        collected.add_later_crawl(
            &seed,
            vec![
                redirected("https://docs.example/docs/r1"),
                redirected("https://docs.example/docs/r2"),
            ],
            0,
        );
        let (result, counts) = collected.finish(false);
        assert_eq!(counts.results_stored, 1);
        assert!(counts.cut_off, "{counts:?}");
        assert_eq!(
            result.expect("stored").missing_policy,
            MissingPagePolicy::Preserve
        );
    }

    #[test]
    fn results_at_limit_count_retired_copies() {
        // `/docs/p/` merges into `/docs/p`: two storable URLs, but three results with the merged copy.
        let html = r#"<html><head><link rel="canonical" href="/docs/p"></head><body><h1>P</h1></body></html>"#;
        let scope = scope();
        let mut collected = CollectedPages::new(&scope, 3);
        collected.add_first_crawl(
            vec![
                typed_page("https://docs.example/docs/p", "text/html", html),
                typed_page("https://docs.example/docs/p/", "text/html", html),
            ],
            0,
        );
        let seed = url("https://docs.example/docs/q");
        assert_eq!(collected.next_later_crawl(&seed), LaterCrawl::Crawl(2));
        collected.add_later_crawl(&seed, vec![page("https://docs.example/docs/q")], 0);
        let (_, counts) = collected.finish(false);
        assert_eq!(counts.canonical_duplicate, 1, "{counts:?}");
        assert!(counts.cut_off, "{counts:?}");
    }

    #[test]
    fn under_budget_is_complete() {
        let (result, counts) = first_crawl(
            vec![
                page("https://docs.example/docs/a"),
                page("https://docs.example/docs/b"),
            ],
            0,
            10,
            false,
        );
        assert!(!counts.cut_off, "{counts:?}");
        assert_eq!(
            result.expect("stored").missing_policy,
            MissingPagePolicy::MarkStale
        );
    }

    #[test]
    fn prune_wins_over_the_cut_off_policy() {
        let pages = || {
            vec![
                page("https://docs.example/docs/a"),
                page("https://docs.example/docs/b"),
            ]
        };
        let policy = |max_pages, prune| {
            first_crawl(pages(), 0, max_pages, prune)
                .0
                .expect("stored")
                .missing_policy
        };
        assert_eq!(policy(2, true), MissingPagePolicy::Prune, "cut off");
        assert_eq!(policy(10, true), MissingPagePolicy::Prune, "complete");
        assert_eq!(policy(2, false), MissingPagePolicy::Preserve);
        assert_eq!(policy(10, false), MissingPagePolicy::MarkStale);
    }

    /// Finish a first crawl of `received` pages, `redirected` of them redirected out of the Source
    /// (`/docs/r1`, `/docs/r2`, ...), and return its warnings.
    fn redirect_warnings(redirected_pages: usize, received: usize, max_pages: u32) -> Vec<String> {
        let mut pages: Vec<Page> = (1..=redirected_pages)
            .map(|i| redirected(&format!("https://docs.example/docs/r{i}")))
            .collect();
        pages.extend(
            (1..=received - redirected_pages)
                .map(|i| page(&format!("https://docs.example/docs/p{i}"))),
        );
        first_crawl(pages, 0, max_pages, false)
            .0
            .expect("stored")
            .warnings
    }

    #[test]
    fn warns_when_budget_went_to_redirects() {
        let warnings = redirect_warnings(5, 20, 20);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        assert!(warning.starts_with("5 of 20 pages received redirected outside this Source"));
        for example in ["/docs/r1", "/docs/r2", "/docs/r3"] {
            assert!(warning.contains(example), "{warning}");
        }
        assert!(!warning.contains("/docs/r4"), "names three: {warning}");
        assert!(warning.contains("max_pages (20)"), "{warning}");
    }

    #[test]
    fn no_warning_below_five_redirects() {
        assert_eq!(redirect_warnings(4, 20, 20), Vec::<String>::new());
    }

    #[test]
    fn no_warning_at_ten_percent() {
        assert_eq!(redirect_warnings(5, 50, 50), Vec::<String>::new());
    }

    #[test]
    fn warns_above_ten_percent() {
        assert_eq!(redirect_warnings(5, 49, 49).len(), 1);
    }

    #[test]
    fn no_warning_when_not_cut_off() {
        assert_eq!(redirect_warnings(5, 20, 100), Vec::<String>::new());
    }

    #[test]
    fn nothing_reachable_fails_with_the_first_error() {
        let mut refused = page("https://docs.example/docs/");
        refused.status_code = StatusCode::INTERNAL_SERVER_ERROR;
        refused.error_status = Some("connection refused".to_string());
        let (result, counts) = first_crawl(vec![refused], 0, 100, false);
        let Err(err) = result else {
            panic!("a Sync with only transport failures fails");
        };
        assert_eq!(
            err.to_string(),
            "HTTP 500 (Internal Server Error): connection refused"
        );
        assert_eq!(counts.results_failed, 1, "the counts survive the error");
    }

    #[test]
    fn a_sync_with_no_pages_fails_naming_the_origin() {
        let (result, _) = first_crawl(Vec::new(), 0, 100, false);
        let Err(err) = result else {
            panic!("a Sync with no pages fails");
        };
        assert_eq!(
            err.to_string(),
            "no reachable pages crawled from 'https://docs.example/docs/'"
        );
    }

    #[test]
    fn an_http_error_page_alone_is_still_a_result() {
        let mut missing = page("https://docs.example/docs/");
        missing.status_code = StatusCode::NOT_FOUND;
        let (result, counts) = first_crawl(vec![missing], 0, 100, false);
        assert_eq!(
            results(&result.expect("a 404 is not a transport failure")),
            vec![("failed", "https://docs.example/docs/")]
        );
        assert_eq!(counts.results_failed, 1);
    }
}
