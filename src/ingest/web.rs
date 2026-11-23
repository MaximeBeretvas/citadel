use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use spider::compact_str::CompactString;
use spider::configuration::{RedirectPolicy, WaitForIdleNetwork, WaitForSelector};
use spider::page::Page;
use spider::website::Website;
use spider_transformations::transformation::content::{
    transform_content, ReturnFormat, TransformConfig,
};
use url::Url;

use crate::db::open_rw_db;
use crate::error::{Result, VaultError};
use crate::ingest::local::{
    fail_sync_run, persist_sync_outcomes, start_sync_run, MissingPagePolicy, PageIngestOutcome,
    PatternMatcher,
};
use crate::ingest::normalize::{count_words, normalize_markdown};
use crate::ingest::sections::split_sections;
use crate::ingest::{SourceRecord, SyncSummary};
use crate::sources::origin::is_private_host;

const DEFAULT_CONCURRENCY: usize = 4;
const DEFAULT_CRAWL_DELAY_MS: u64 = 100;
const USER_AGENT: &str = concat!("citadel/", env!("CARGO_PKG_VERSION"));
/// Per-page cap on waiting for a JS-rendered page to settle before links are read.
const RENDER_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const SEED_FETCH_TIMEOUT_SECS: u64 = 10;
const MAX_CHILD_SITEMAPS: usize = 10;
const NO_CHROME_WARNING: &str = "render_js is enabled, but citadel was built without `--features chrome`; fell back to static HTTP crawl";

const STATIC_ASSET_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "tiff", "avif", "css", "js", "mjs",
    "cjs", "map", "woff", "woff2", "ttf", "otf", "eot", "mp3", "mp4", "webm", "ogg", "wav", "avi",
    "mov", "pdf", "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "exe", "dmg", "pkg", "deb",
    "rpm", "wasm",
];

#[derive(Debug, Clone)]
struct WebScope {
    origin: Url,
    origin_prefix: String,
    origin_scheme: String,
    origin_host: String,
    origin_port: Option<u16>,
    has_includes: bool,
    matcher: PatternMatcher,
    include_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
}

impl WebScope {
    fn new(origin: &Url, include_patterns: &[String], exclude_patterns: &[String]) -> Result<Self> {
        let host = origin
            .host_str()
            .ok_or_else(|| VaultError::Validation(format!("origin URL '{origin}' lacks a host")))?
            .to_ascii_lowercase();
        let matcher = PatternMatcher::new(include_patterns, exclude_patterns)?;

        Ok(Self {
            origin: origin.clone(),
            origin_prefix: normalize_prefix(origin.path()),
            origin_scheme: origin.scheme().to_ascii_lowercase(),
            origin_host: host,
            origin_port: origin.port_or_known_default(),
            has_includes: !include_patterns.is_empty(),
            matcher,
            include_patterns: include_patterns.to_vec(),
            exclude_patterns: exclude_patterns.to_vec(),
        })
    }

    fn same_authority(&self, url: &Url) -> bool {
        let scheme_ok = url.scheme().eq_ignore_ascii_case(&self.origin_scheme);
        let host_ok = url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&self.origin_host));
        let port_ok = url.port_or_known_default() == self.origin_port;
        scheme_ok && host_ok && port_ok
    }

    fn matches_prefix(&self, url: &Url) -> bool {
        matches_url_prefix(&self.origin_prefix, url.path())
    }

    fn matches_pattern_candidates(&self, url: &Url, matcher: &PatternMatcher) -> bool {
        let root_rel = url.path().trim_start_matches('/');
        if matcher.matches(root_rel) || matcher.matches(url.as_str()) {
            return true;
        }
        if let Some(origin_rel) = self.origin_relative_path(url.path()) {
            if !origin_rel.is_empty() && matcher.matches(origin_rel) {
                return true;
            }
        }
        false
    }

    fn is_excluded(&self, url: &Url) -> bool {
        if self.exclude_patterns.is_empty() {
            return false;
        }
        let Ok(exclude_only) = PatternMatcher::new(&[], &self.exclude_patterns) else {
            return false;
        };
        let root_rel = url.path().trim_start_matches('/');
        if !exclude_only.matches(root_rel) || !exclude_only.matches(url.as_str()) {
            return true;
        }
        if let Some(origin_rel) = self.origin_relative_path(url.path()) {
            if !origin_rel.is_empty() && !exclude_only.matches(origin_rel) {
                return true;
            }
        }
        false
    }

    fn is_included(&self, url: &Url) -> bool {
        if !self.has_includes {
            return self.matches_prefix(url);
        }
        let Ok(include_only) = PatternMatcher::new(&self.include_patterns, &[]) else {
            return false;
        };
        self.matches_pattern_candidates(url, &include_only)
    }

    fn should_crawl(&self, url: &Url) -> bool {
        if !self.same_authority(url) || is_static_asset_url(url) || self.is_excluded(url) {
            return false;
        }
        if !self.has_includes {
            return self.matches_prefix(url);
        }
        canonical_dedup_key(url) == canonical_dedup_key(&self.origin) || self.is_included(url)
    }

    fn should_store(&self, url: &Url) -> bool {
        if !self.same_authority(url) || is_static_asset_url(url) || self.is_excluded(url) {
            return false;
        }
        if !self.has_includes {
            return self.matches_prefix(url);
        }
        self.is_included(url)
    }

    fn origin_relative_path<'a>(&self, candidate_path: &'a str) -> Option<&'a str> {
        if self.origin_prefix == "/" {
            return Some(candidate_path.trim_start_matches('/'));
        }
        if let Some(rest) = candidate_path.strip_prefix(&self.origin_prefix) {
            if rest.is_empty() || rest == "/" {
                return Some("");
            }
            if let Some(sub) = rest.strip_prefix('/') {
                return Some(sub);
            }
        }
        None
    }

    fn authority_regex(&self) -> String {
        let scheme = escape_regex_literal(&self.origin_scheme);
        let host = escape_regex_literal(&self.origin_host);
        let port = match self.origin.port() {
            Some(p) => format!(":{p}"),
            None => String::new(),
        };
        format!(r"{scheme}://{host}{port}")
    }

    fn whitelist_regexes(&self) -> Vec<CompactString> {
        let auth = self.authority_regex();
        if !self.has_includes {
            let re = if self.origin_prefix == "/" {
                format!(r"^{auth}(/|$|\?|#)")
            } else {
                format!(
                    r"^{auth}{}(/|$|\?|#)",
                    escape_regex_literal(&self.origin_prefix)
                )
            };
            return vec![re.into()];
        }

        let mut list = Vec::with_capacity(self.include_patterns.len() + 1);
        let origin_path = self.origin.path().trim_end_matches('/');
        if origin_path.is_empty() {
            list.push(CompactString::from(format!(r"^{auth}/?($|\?|#)")));
        } else {
            list.push(CompactString::from(format!(
                r"^{auth}{}/?($|\?|#)",
                escape_regex_literal(origin_path)
            )));
        }
        for pat in &self.include_patterns {
            list.extend(
                pattern_to_url_regexes(&auth, &self.origin_prefix, pat)
                    .into_iter()
                    .map(CompactString::from),
            );
        }
        list
    }

    fn blacklist_regexes(&self) -> Vec<CompactString> {
        let auth = self.authority_regex();
        let mut list = vec![CompactString::from(
            r"\.(png|jpe?g|gif|webp|svg|ico|bmp|tiff|avif|css|js|mjs|cjs|map|woff2?|ttf|otf|eot|mp3|mp4|webm|ogg|wav|avi|mov|pdf|zip|tar|gz|tgz|bz2|xz|7z|rar|exe|dmg|pkg|deb|rpm|wasm)(\?.*)?$",
        )];
        for pat in &self.exclude_patterns {
            list.extend(
                pattern_to_url_regexes(&auth, &self.origin_prefix, pat)
                    .into_iter()
                    .map(CompactString::from),
            );
        }
        list
    }
}

/// Synchronize a web documentation source using `spider` and `spider_transformations`.
pub async fn sync_web_source(
    db_path: &Path,
    source: &SourceRecord,
    prune: bool,
) -> Result<SyncSummary> {
    let origin = Url::parse(&source.origin).map_err(|err| {
        VaultError::Validation(format!("invalid web source URL '{}': {err}", source.origin))
    })?;
    if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
        return Err(VaultError::Validation(format!(
            "web source URL '{}' must use http:// or https:// with a valid host",
            source.origin
        )));
    }

    let scope = WebScope::new(&origin, &source.include_patterns, &source.exclude_patterns)?;
    // Touch matcher field to verify pattern compilation cleanly up front.
    let _ = &scope.matcher;

    let run_id = {
        let conn = open_rw_db(db_path)?;
        start_sync_run(&conn, source.id)?
    };

    let crawl_res = execute_web_crawl(source, &origin, &scope).await;

    let mut conn = open_rw_db(db_path)?;
    match crawl_res {
        Ok((outcomes, warnings)) => {
            let missing_policy = if prune {
                MissingPagePolicy::Prune
            } else if outcomes.len() >= source.max_pages as usize {
                MissingPagePolicy::Preserve
            } else {
                MissingPagePolicy::MarkStale
            };
            persist_sync_outcomes(
                &mut conn,
                source.id,
                &source.name,
                run_id,
                outcomes,
                missing_policy,
                warnings,
            )
        }
        Err(err) => {
            fail_sync_run(&conn, source.id, run_id, &err.to_string());
            Err(err)
        }
    }
}

async fn execute_web_crawl(
    source: &SourceRecord,
    origin: &Url,
    scope: &WebScope,
) -> Result<(Vec<PageIngestOutcome>, Vec<String>)> {
    let mut warnings = Vec::new();
    if source.render_js && !cfg!(feature = "chrome") {
        tracing::warn!(source = %source.name, "{}", NO_CHROME_WARNING);
        warnings.push(NO_CHROME_WARNING.to_string());
    }

    let seed_urls = discover_seed_urls(origin, scope).await;

    let mut collected_pages: Vec<Page> = Vec::new();
    let mut visited_keys: HashSet<String> = HashSet::new();

    let mut primary = configure_website(
        origin,
        scope,
        source.max_pages,
        source.max_depth,
        source.render_js,
    );
    let primary_pages = run_website_crawl(&mut primary, source.render_js).await?;
    for page in primary_pages {
        if let Some(key) = page_dedup_key(&page) {
            visited_keys.insert(key);
        }
        collected_pages.push(page);
    }

    for seed in seed_urls {
        let used_budget = count_scoped_pages(&collected_pages, scope);
        if used_budget >= source.max_pages as usize {
            break;
        }
        let seed_key = canonical_dedup_key(&seed);
        if visited_keys.contains(&seed_key) {
            continue;
        }

        let remaining = source.max_pages.saturating_sub(used_budget as u32);
        if remaining == 0 {
            break;
        }

        let mut seed_site =
            configure_website(&seed, scope, remaining, source.max_depth, source.render_js);
        let extra_pages = run_website_crawl(&mut seed_site, source.render_js).await?;
        for page in extra_pages {
            let Some(key) = page_dedup_key(&page) else {
                continue;
            };
            if visited_keys.insert(key) {
                collected_pages.push(page);
            }
        }
        visited_keys.insert(seed_key);
    }

    let (outcomes, ok_count, transport_failures) = build_page_outcomes(collected_pages, scope);

    if ok_count == 0 && (outcomes.is_empty() || transport_failures == outcomes.len()) {
        let reason = outcomes
            .iter()
            .find_map(|o| match o {
                PageIngestOutcome::Failed { error_message, .. } => Some(error_message.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("no reachable pages crawled from '{}'", source.origin));
        return Err(VaultError::Other(reason));
    }

    Ok((outcomes, warnings))
}

fn configure_website(
    seed: &Url,
    scope: &WebScope,
    max_pages: u32,
    max_depth: u32,
    render_js: bool,
) -> Website {
    let mut website = Website::new_with_firewall(seed.as_str(), false);
    let base_segments = seed.path().split('/').filter(|s| !s.is_empty()).count();
    let effective_depth = base_segments.saturating_add(max_depth as usize).max(1);

    let redirect_policy = if is_private_host(seed) {
        RedirectPolicy::Strict
    } else {
        RedirectPolicy::Loose
    };

    website
        .with_respect_robots_txt(true)
        .with_limit(max_pages)
        .with_depth(effective_depth)
        .with_concurrency_limit(Some(DEFAULT_CONCURRENCY))
        .with_delay(DEFAULT_CRAWL_DELAY_MS)
        .with_subdomains(false)
        .with_tld(false)
        .with_redirect_policy(redirect_policy)
        .with_whitelist_url(Some(scope.whitelist_regexes()))
        .with_blacklist_url(Some(scope.blacklist_regexes()))
        .with_user_agent(Some(USER_AGENT));

    if render_js {
        // Script-built navigation (e.g. sidebars rendered from embedded JSON) only exists once
        // the page settles; read links after the network and DOM go quiet, capped per page.
        website
            .with_wait_for_idle_network0(Some(WaitForIdleNetwork::new(Some(RENDER_WAIT_TIMEOUT))))
            .with_wait_for_idle_dom(Some(WaitForSelector::new(
                Some(RENDER_WAIT_TIMEOUT),
                "body".to_string(),
            )));
    }

    website
}

/// Fail a `render_js` Sync up front when the configured remote browser cannot be reached: spider
/// would otherwise fall back to a static fetch per page and silently miss script-built links.
#[cfg(feature = "chrome")]
async fn ensure_render_browser(website: &Website) -> Result<()> {
    match website.setup_browser().await {
        Some(mut browser) => {
            browser.dispose();
            Ok(())
        }
        None => Err(VaultError::Other(
            super::browser::NO_BROWSER_ERROR.to_string(),
        )),
    }
}

/// Give a `render_js` crawl its own browser: a remote one from `CHROME_URL` when set (checked
/// up front), otherwise a private headless Chrome launched for this crawl only.
#[cfg(feature = "chrome")]
async fn prepare_render_browser(
    website: &mut Website,
) -> Result<Option<super::browser::RenderBrowser>> {
    if std::env::var_os(super::browser::CHROME_URL_ENV).is_some() {
        ensure_render_browser(website).await?;
        return Ok(None);
    }
    let browser = super::browser::RenderBrowser::launch().await?;
    website.with_chrome_connection(Some(browser.websocket_url()));
    Ok(Some(browser))
}

async fn run_website_crawl(website: &mut Website, render_js: bool) -> Result<Vec<Page>> {
    #[cfg(feature = "chrome")]
    let render_browser = if render_js {
        prepare_render_browser(website).await?
    } else {
        None
    };

    let mut rx = website.subscribe(256);
    let collector = tokio::spawn(async move {
        let mut pages = Vec::new();
        loop {
            match rx.recv().await {
                Ok(page) => pages.push(page),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "spider broadcast channel lagged during crawl");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        pages
    });

    if render_js && cfg!(feature = "chrome") {
        website.crawl().await;
    } else {
        website.crawl_raw().await;
    }
    website.unsubscribe();

    #[cfg(feature = "chrome")]
    if let Some(browser) = render_browser {
        browser.shutdown().await;
    }

    collector
        .await
        .map_err(|err| VaultError::Other(format!("crawler collector task panicked: {err}")))
}

fn count_scoped_pages(pages: &[Page], scope: &WebScope) -> usize {
    let mut unique = HashSet::new();
    for page in pages {
        let Some(url) = canonicalize_page_url(page.get_url()) else {
            continue;
        };
        if scope.should_store(&url) {
            unique.insert(canonical_dedup_key(&url));
        }
    }
    unique.len()
}

fn build_page_outcomes(
    pages: Vec<Page>,
    scope: &WebScope,
) -> (Vec<PageIngestOutcome>, usize, usize) {
    let mut outcomes = Vec::new();
    let mut seen_uris = HashSet::new();
    let mut ok_count = 0usize;
    let mut transport_failures = 0usize;

    for page in pages {
        let Some(canonical_url) = canonicalize_page_url(page.get_url()) else {
            continue;
        };

        if let Some(final_dest) = page.final_redirect_destination.as_deref() {
            if let Some(dest_url) = canonicalize_page_url(final_dest) {
                if !scope.should_crawl(&dest_url) {
                    continue;
                }
            }
        }

        if !scope.should_store(&canonical_url) {
            continue;
        }

        let uri = canonical_url.to_string();
        if !seen_uris.insert(uri.clone()) {
            continue;
        }

        let status_u16 = page.status_code.as_u16();
        if status_u16 >= 400 || status_u16 == 0 || page.error_status.is_some() {
            let is_transport = status_u16 == 0 || status_u16 >= 520 || page.error_status.is_some();
            if is_transport {
                transport_failures += 1;
            }
            let error_message = format_page_error(&page, status_u16);
            outcomes.push(PageIngestOutcome::Failed {
                uri,
                http_status: (status_u16 > 0).then_some(i64::from(status_u16)),
                error_message,
                insert_if_missing: true,
            });
            continue;
        }

        if page.binary_file {
            continue;
        }

        let (raw_md, html_title) = extract_page_markdown_and_title(&page, &canonical_url);
        let fallback_title = html_title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| fallback_title_from_url(&canonical_url));

        let doc = normalize_markdown(&raw_md, &fallback_title);
        let chunks = split_sections(&doc.content_md);
        let (etag, last_modified) = extract_http_validators(&page);

        ok_count += 1;
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

async fn discover_seed_urls(origin: &Url, scope: &WebScope) -> Vec<Url> {
    let client = match build_seed_http_client(origin) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build HTTP client for llms.txt/sitemap seeding");
            return Vec::new();
        }
    };

    let mut seeds = Vec::new();
    let mut seen = HashSet::new();

    for llms_url in build_root_candidates(origin, "llms.txt") {
        if let Some(body) = fetch_text_if_ok(&client, &llms_url).await {
            for candidate in parse_llms_txt(&body, &llms_url) {
                if scope.should_crawl(&candidate) && seen.insert(canonical_dedup_key(&candidate)) {
                    seeds.push(candidate);
                }
            }
        }
    }

    let mut sitemap_queue = build_root_candidates(origin, "sitemap.xml");
    let mut visited_sitemaps = HashSet::new();
    let mut child_fetches = 0usize;

    while let Some(sitemap_url) = sitemap_queue.pop() {
        if !visited_sitemaps.insert(sitemap_url.to_string()) {
            continue;
        }
        let Some(body) = fetch_text_if_ok(&client, &sitemap_url).await else {
            continue;
        };
        let (page_urls, child_sitemaps) = parse_sitemap_xml(&body, &sitemap_url);
        for candidate in page_urls {
            if scope.should_crawl(&candidate) && seen.insert(canonical_dedup_key(&candidate)) {
                seeds.push(candidate);
            }
        }
        for child in child_sitemaps {
            if child_fetches >= MAX_CHILD_SITEMAPS {
                break;
            }
            if scope.same_authority(&child) {
                child_fetches += 1;
                sitemap_queue.push(child);
            }
        }
    }

    seeds
}

fn build_seed_http_client(origin: &Url) -> std::result::Result<reqwest::Client, reqwest::Error> {
    let allow_local = is_private_host(origin);
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 5 {
            return attempt.error("too many redirects");
        }
        if !allow_local && is_private_host(attempt.url()) {
            return attempt.error("SSRF blocked: redirect to internal address");
        }
        attempt.follow()
    });

    reqwest::Client::builder()
        .timeout(Duration::from_secs(SEED_FETCH_TIMEOUT_SECS))
        .user_agent("citadel/0.1")
        .redirect(policy)
        .build()
}

async fn fetch_text_if_ok(client: &reqwest::Client, url: &Url) -> Option<String> {
    let resp = client.get(url.as_str()).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

fn build_root_candidates(origin: &Url, filename: &str) -> Vec<Url> {
    let mut out = Vec::new();
    if let Ok(mut root) = Url::parse(origin.as_str()) {
        root.set_query(None);
        root.set_fragment(None);
        root.set_path(&format!("/{filename}"));
        out.push(root);
    }

    let prefix = normalize_prefix(origin.path());
    if prefix != "/" {
        if let Ok(mut sub) = Url::parse(origin.as_str()) {
            sub.set_query(None);
            sub.set_fragment(None);
            sub.set_path(&format!("{prefix}/{filename}"));
            if !out.contains(&sub) {
                out.push(sub);
            }
        }
    }

    out
}

pub(crate) fn parse_llms_txt(body: &str, base_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();

    // 1. Markdown `[label](target)` links
    let mut rest = body;
    while let Some(idx) = rest.find("](") {
        let after_open = &rest[idx + 2..];
        let Some(close_idx) = after_open.find(')') else {
            break;
        };
        let inside = after_open[..close_idx].trim();
        let target = extract_markdown_link_target(inside);
        if let Some(resolved) = resolve_seed_url(base_url, target) {
            if seen.insert(resolved.to_string()) {
                urls.push(resolved);
            }
        }
        rest = &after_open[close_idx + 1..];
    }

    // 2. Bare http(s):// URLs and root-relative paths on list lines
    for line in body.lines() {
        let trimmed = line.trim();
        let list_item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim);
        if let Some(item) = list_item {
            if item.starts_with('/') {
                let path_token = item.split_whitespace().next().unwrap_or("");
                if let Some(resolved) = resolve_seed_url(base_url, path_token) {
                    if seen.insert(resolved.to_string()) {
                        urls.push(resolved);
                    }
                }
            }
        }

        for token in trimmed.split_whitespace() {
            let cleaned = token
                .trim_matches(['<', '>', '(', ')', '[', ']', '"', '\''])
                .trim_end_matches(['.', ',', ';', ':']);
            if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
                if let Some(resolved) = resolve_seed_url(base_url, cleaned) {
                    if seen.insert(resolved.to_string()) {
                        urls.push(resolved);
                    }
                }
            }
        }
    }

    urls
}

fn extract_markdown_link_target(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed.strip_prefix('<') {
        if let Some(end) = inner.find('>') {
            return inner[..end].trim();
        }
    }
    trimmed.split_whitespace().next().unwrap_or("")
}

fn parse_sitemap_xml(body: &str, base_url: &Url) -> (Vec<Url>, Vec<Url>) {
    let mut pages = Vec::new();
    let mut sitemaps = Vec::new();
    let is_index = body.contains("<sitemapindex");

    let mut cursor = body;
    while let Some(start) = cursor.find("<loc") {
        let after_tag_start = &cursor[start + 4..];
        let Some(gt) = after_tag_start.find('>') else {
            break;
        };
        let after_gt = &after_tag_start[gt + 1..];
        let Some(end) = after_gt.find("</loc>") else {
            break;
        };
        let raw_loc = after_gt[..end].trim();
        let unescaped = raw_loc
            .strip_prefix("<![CDATA[")
            .and_then(|s| s.strip_suffix("]]>"))
            .unwrap_or(raw_loc)
            .replace("&amp;", "&");
        if let Some(resolved) = resolve_seed_url(base_url, unescaped.trim()) {
            if is_index {
                sitemaps.push(resolved);
            } else {
                pages.push(resolved);
            }
        }
        cursor = &after_gt[end + 6..];
    }

    (pages, sitemaps)
}

fn resolve_seed_url(base_url: &Url, raw_target: &str) -> Option<Url> {
    let trimmed = raw_target.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("javascript:")
    {
        return None;
    }
    let mut joined = base_url.join(trimmed).ok()?;
    if !matches!(joined.scheme(), "http" | "https") {
        return None;
    }
    joined.set_fragment(None);
    Some(joined)
}

fn canonicalize_page_url(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

fn canonical_dedup_key(url: &Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    let path = normalized.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        normalized.set_path(path.trim_end_matches('/'));
    }
    normalized.to_string()
}

fn page_dedup_key(page: &Page) -> Option<String> {
    canonicalize_page_url(page.get_url()).map(|u| canonical_dedup_key(&u))
}

fn normalize_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn matches_url_prefix(prefix: &str, candidate_path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    let clean_candidate = candidate_path.trim_end_matches('/');
    clean_candidate == prefix
        || candidate_path == prefix
        || candidate_path.starts_with(&format!("{prefix}/"))
}

fn is_static_asset_url(url: &Url) -> bool {
    let Some(last) = url.path_segments().and_then(|mut s| s.next_back()) else {
        return false;
    };
    let Some((_, ext)) = last.rsplit_once('.') else {
        return false;
    };
    STATIC_ASSET_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
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

fn escape_regex_literal(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn glob_to_regex(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                if let Some(close_rel) = chars[i + 1..].iter().position(|c| *c == ']') {
                    out.push('[');
                    for ch in &chars[i + 1..=i + 1 + close_rel] {
                        out.push(*ch);
                    }
                    i += 1 + close_rel + 1;
                    continue;
                }
                out.push_str(r"\[");
            }
            ch if matches!(
                ch,
                '\\' | '.' | '+' | '(' | ')' | '|' | ']' | '{' | '}' | '^' | '$'
            ) =>
            {
                out.push('\\');
                out.push(ch);
            }
            ch => out.push(ch),
        }
        i += 1;
    }
    out
}

fn pattern_to_url_regexes(authority_re: &str, origin_prefix: &str, raw_pat: &str) -> Vec<String> {
    let trimmed = raw_pat.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let body = if trimmed.contains(['*', '?', '[']) {
            glob_to_regex(trimmed)
        } else {
            format!(
                "{}(/|$|\\?|#)",
                escape_regex_literal(trimmed.trim_end_matches('/'))
            )
        };
        return vec![format!("^{body}")];
    }

    let rel = trimmed.trim_start_matches("./").trim_start_matches('/');
    let suffix_re = if rel.contains(['*', '?', '[']) {
        glob_to_regex(rel)
    } else {
        format!(
            "{}(/|$|\\?|#)",
            escape_regex_literal(rel.trim_end_matches('/'))
        )
    };

    let mut out = vec![format!(r"^{authority_re}/{suffix_re}")];
    if origin_prefix != "/" {
        let prefix_escaped = escape_regex_literal(origin_prefix);
        out.push(format!(r"^{authority_re}{prefix_escaped}/{suffix_re}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::{Arc, RwLock};

    use axum::extract::State;
    use axum::http::{header, StatusCode};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    use crate::config::VaultConfig;
    use crate::db::open_rw_db;
    use crate::ingest::SourceKind;
    use crate::sources::{AddSourceParams, Added, SourceLifecycle};

    type FixtureRouteMap = Arc<RwLock<HashMap<String, (StatusCode, String, String)>>>;

    #[derive(Clone, Default)]
    struct FixtureState {
        routes: FixtureRouteMap,
    }

    impl FixtureState {
        fn set_html(&self, path: &str, status: StatusCode, body: &str) {
            let mut guard = self.routes.write().expect("lock routes");
            guard.insert(
                path.to_string(),
                (
                    status,
                    "text/html; charset=utf-8".to_string(),
                    body.to_string(),
                ),
            );
        }

        fn set_text(&self, path: &str, status: StatusCode, body: &str) {
            let mut guard = self.routes.write().expect("lock routes");
            guard.insert(
                path.to_string(),
                (
                    status,
                    "text/plain; charset=utf-8".to_string(),
                    body.to_string(),
                ),
            );
        }

        fn set_xml(&self, path: &str, status: StatusCode, body: &str) {
            let mut guard = self.routes.write().expect("lock routes");
            guard.insert(
                path.to_string(),
                (
                    status,
                    "application/xml; charset=utf-8".to_string(),
                    body.to_string(),
                ),
            );
        }

        fn remove(&self, path: &str) {
            let mut guard = self.routes.write().expect("lock routes");
            guard.remove(path);
        }
    }

    async fn serve_fixture_route(
        State(state): State<FixtureState>,
        uri: axum::http::Uri,
    ) -> Response {
        let path = uri.path().to_string();
        let entry = {
            let guard = state.routes.read().expect("lock routes");
            guard.get(&path).cloned()
        };
        match entry {
            Some((status, content_type, body)) => (
                status,
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::ETAG, format!("\"etag-{path}\"")),
                    (
                        header::LAST_MODIFIED,
                        "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
                    ),
                ],
                body,
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, Html("<h1>404 Not Found</h1>")).into_response(),
        }
    }

    async fn spawn_fixture_server(
        state: FixtureState,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/", get(serve_fixture_route))
            .route("/{*path}", get(serve_fixture_route))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local_addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn crawls_local_fixture_site_within_prefix_and_extracts_markdown() {
        let state = FixtureState::default();
        state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<!doctype html>
<html>
<head>
  <title>Docs Home</title>
  <style>.banner { display: none; /* noise-style-token */ }</style>
</head>
<body>
  <nav><a href="/blog/off-topic">Blog</a><a href="/docs-extra">Extra</a></nav>
  <main>
    <h1>Documentation</h1>
    <p>Welcome to the documentation hub with detailed reference material.</p>
    <ul>
      <li><a href="/docs/guide">Guide</a></li>
      <li><a href="/docs/api">API</a></li>
    </ul>
  </main>
  <footer>Site Footer Noise</footer>
  <script>console.log("noise-script-token");</script>
</body>
</html>"#,
        );
        state.set_html(
            "/docs/guide",
            StatusCode::OK,
            r#"<!doctype html>
<html>
<head><title>User Guide</title></head>
<body>
  <nav><a href="/blog/off-topic">Blog</a></nav>
  <main>
    <h1>User Guide</h1>
    <p>Getting started steps for setting up your local vault quickly.</p>
    <h2>Configuration</h2>
    <p>Set environment variables cleanly before running sync.</p>
  </main>
  <footer>Guide Footer Noise</footer>
</body>
</html>"#,
        );
        state.set_html(
            "/docs/api",
            StatusCode::OK,
            r#"<!doctype html>
<html>
<head><title>API Reference</title></head>
<body>
  <main>
    <h1>API Reference</h1>
    <p>HTTP endpoints reference for querying indexed documents.</p>
    <h3>Authentication</h3>
    <p>Use bearer tokens for remote API requests.</p>
  </main>
</body>
</html>"#,
        );
        state.set_html(
            "/blog/off-topic",
            StatusCode::OK,
            "<html><body><h1>Off Topic Blog</h1><p>Should never be ingested.</p></body></html>",
        );
        state.set_html(
            "/docs-extra",
            StatusCode::OK,
            "<html><body><h1>Docs Extra</h1><p>Sibling prefix must not be ingested.</p></body></html>",
        );

        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let origin = format!("http://{addr}/docs/");

        let Added {
            source,
            summary: summary_opt,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: origin.clone(),
                    name: Some("fixture-docs".to_string()),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add should succeed");
        server_handle.abort();

        assert_eq!(source.kind, SourceKind::Web);
        assert_eq!(source.status, "ok");

        let summary = summary_opt.expect("sync summary should be present");
        assert_eq!(summary.source_name, "fixture-docs");
        assert_eq!(summary.pages_added, 3);
        assert_eq!(summary.pages_failed, 0);
        assert_eq!(summary.pages_stale, 0);

        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");
        let rows: Vec<(String, String, String, String)> = conn
            .prepare(
                "SELECT uri, title, content_md, status
                 FROM pages
                 WHERE source_id = ?1
                 ORDER BY uri ASC",
            )
            .expect("prepare")
            .query_map([source.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 3);
        let uris: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(uris.contains(&format!("http://{addr}/docs/").as_str()));
        assert!(uris.contains(&format!("http://{addr}/docs/guide").as_str()));
        assert!(uris.contains(&format!("http://{addr}/docs/api").as_str()));
        assert!(!uris.iter().any(|u| u.contains("/blog/off-topic")));
        assert!(!uris.iter().any(|u| u.contains("/docs-extra")));

        for (uri, title, md, status) in &rows {
            assert_eq!(status, "active", "page {uri} should be active");
            assert!(!title.trim().is_empty(), "page {uri} must have title");
            assert!(
                !md.contains("Footer Noise")
                    && !md.contains("noise-script-token")
                    && !md.contains("noise-style-token"),
                "page {uri} leaked stripped noise into markdown: {md}"
            );
        }

        let section_paths: Vec<String> = conn
            .prepare("SELECT heading_path FROM sections WHERE source_id = ?1 ORDER BY id ASC")
            .expect("prepare sections")
            .query_map([source.id], |r| r.get(0))
            .expect("query sections")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect sections");

        assert!(
            section_paths
                .iter()
                .any(|p| p == "User Guide > Configuration"),
            "expected H1 > H2 breadcrumb in sections, got: {section_paths:?}"
        );
        assert!(
            section_paths
                .iter()
                .any(|p| p == "API Reference > Authentication"),
            "expected H1 > H3 breadcrumb in sections, got: {section_paths:?}"
        );
    }

    #[tokio::test]
    async fn seeds_urls_from_llms_txt_when_present() {
        let state = FixtureState::default();
        let (addr, server_handle) = spawn_fixture_server(state.clone()).await;

        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><head><title>Docs</title></head><body><main><h1>Docs</h1><p>Index page with link to guide only.</p><a href="/docs/guide">Guide</a></main></body></html>"#,
        );
        state.set_html(
            "/docs/guide",
            StatusCode::OK,
            r#"<html><head><title>Guide</title></head><body><main><h1>Guide</h1><p>Standard linked guide content.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/hidden",
            StatusCode::OK,
            r#"<html><head><title>Hidden Doc</title></head><body><main><h1>Hidden Guide</h1><p>Unlinked page discovered via llms.txt tokenllms.</p></main></body></html>"#,
        );
        state.set_html(
            "/blog/secret",
            StatusCode::OK,
            r#"<html><body><h1>Secret Blog</h1><p>Out of prefix scope.</p></body></html>"#,
        );
        state.set_text(
            "/llms.txt",
            StatusCode::OK,
            &format!(
                "# Fixture Docs\n\
                 - [Guide](/docs/guide): Main guide\n\
                 - [Hidden](http://{addr}/docs/hidden): Hidden page\n\
                 - [Blog](/blog/secret): Out of scope\n"
            ),
        );

        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let origin = format!("http://{addr}/docs/");

        let Added {
            source,
            summary: summary_opt,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin,
                    name: Some("llms-docs".to_string()),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add should succeed");
        server_handle.abort();

        let summary = summary_opt.expect("sync summary");
        assert_eq!(summary.pages_added, 3);

        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");
        let uris: Vec<String> = conn
            .prepare("SELECT uri FROM pages WHERE source_id = ?1 ORDER BY uri")
            .expect("prepare")
            .query_map([source.id], |r| r.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert!(uris.contains(&format!("http://{addr}/docs/hidden")));
        assert!(!uris.iter().any(|u| u.contains("/blog/secret")));

        let fts_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'tokenllms'",
                [],
                |r| r.get(0),
            )
            .expect("query sections_fts");
        assert_eq!(fts_hits, 1);
    }

    #[tokio::test]
    async fn recrawl_records_diff_marks_removed_stale_and_500_error() {
        let state = FixtureState::default();
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><head><title>Docs</title></head><body><nav><a href="/docs/guide">Guide</a><a href="/docs/api">API</a><a href="/docs/broken">Broken</a></nav><main><h1>Docs</h1><p>Stable home page.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/guide",
            StatusCode::OK,
            r#"<html><head><title>Guide</title></head><body><main><h1>Guide</h1><p>Initial guide paragraph for v1.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/api",
            StatusCode::OK,
            r#"<html><head><title>API</title></head><body><main><h1>API</h1><p>API endpoints v1.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/broken",
            StatusCode::OK,
            r#"<html><head><title>Broken</title></head><body><main><h1>Broken Page</h1><p>Healthy cached content before outage.</p></main></body></html>"#,
        );

        let (addr, server_handle) = spawn_fixture_server(state.clone()).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let origin = format!("http://{addr}/docs/");

        let Added {
            source,
            summary: first_sum,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin,
                    name: Some("recrawl-docs".to_string()),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("initial crawl");
        assert_eq!(first_sum.expect("first summary").pages_added, 4);

        // Phase 2: modify /docs/guide, unlink & remove /docs/api, return 500 on /docs/broken
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><head><title>Docs</title></head><body><nav><a href="/docs/guide">Guide</a><a href="/docs/broken">Broken</a></nav><main><h1>Docs</h1><p>Stable home page.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/guide",
            StatusCode::OK,
            r#"<html><head><title>Guide</title></head><body><main><h1>Guide</h1><p>Updated guide paragraph for v2 with new details.</p></main></body></html>"#,
        );
        state.remove("/docs/api");
        state.set_html(
            "/docs/broken",
            StatusCode::INTERNAL_SERVER_ERROR,
            "<html><body><h1>500 Internal Server Error</h1></body></html>",
        );

        let second = SourceLifecycle::new(cfg.clone())
            .sync("recrawl-docs", false)
            .await
            .expect("recrawl sync")
            .synced()
            .expect("synced");
        server_handle.abort();

        assert_eq!(second.pages_added, 0);
        assert_eq!(second.pages_updated, 1);
        assert_eq!(second.pages_unchanged, 1);
        assert_eq!(second.pages_stale, 1);
        assert_eq!(second.pages_failed, 1);

        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");

        // Verify page_revisions diff for /docs/guide
        let diff: String = conn
            .query_row(
                "SELECT pr.unified_diff
                 FROM page_revisions pr
                 JOIN pages p ON p.id = pr.page_id
                 WHERE p.source_id = ?1 AND p.uri = ?2",
                rusqlite::params![source.id, format!("http://{addr}/docs/guide")],
                |r| r.get(0),
            )
            .expect("page_revisions row for /docs/guide");
        assert!(diff.contains("-Initial guide paragraph for v1."));
        assert!(diff.contains("+Updated guide paragraph for v2 with new details."));

        // Verify /docs/api is marked stale
        let api_status: String = conn
            .query_row(
                "SELECT status FROM pages WHERE source_id = ?1 AND uri = ?2",
                rusqlite::params![source.id, format!("http://{addr}/docs/api")],
                |r| r.get(0),
            )
            .expect("api status");
        assert_eq!(api_status, "stale");

        // Verify /docs/broken is marked error with http_status=500 and preserved content_md
        let (broken_status, broken_http, broken_err, broken_md): (
            String,
            Option<i64>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT status, http_status, error_message, content_md
                 FROM pages
                 WHERE source_id = ?1 AND uri = ?2",
                rusqlite::params![source.id, format!("http://{addr}/docs/broken")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("broken page row");
        assert_eq!(broken_status, "error");
        assert_eq!(broken_http, Some(500));
        assert!(broken_err.is_some());
        assert!(
            broken_md.contains("Healthy cached content before outage."),
            "previous content_md must be preserved on error, got: {broken_md}"
        );
    }

    #[tokio::test]
    async fn render_js_without_chrome_feature_returns_clear_warning() {
        let state = FixtureState::default();
        state.set_html(
            "/docs/",
            StatusCode::OK,
            "<html><head><title>Static Page</title></head><body><main><h1>Static Page</h1><p>Static HTML fallback body.</p></main></body></html>",
        );

        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let origin = format!("http://{addr}/docs/");

        let Added {
            source,
            summary: summary_opt,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin,
                    name: Some("js-docs".to_string()),
                    render_js: true,
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add should fall back or succeed gracefully");
        server_handle.abort();

        assert!(source.render_js);
        let summary = summary_opt.expect("summary");

        if !cfg!(feature = "chrome") {
            assert_eq!(summary.pages_added, 1);
            assert!(
                !summary.warnings.is_empty(),
                "expected actionable warning when render_js=true without chrome feature"
            );
            let combined = summary.warnings.join("\n").to_ascii_lowercase();
            assert!(
                combined.contains("chrome"),
                "warning must explain chrome feature requirement, got: {combined}"
            );
        }
    }

    /// Fixture site whose only links to `/docs/a` and `/docs/b` are added by JavaScript after a
    /// short delay, like DevNet PubHub builds its sidebar from embedded JSON.
    fn js_links_fixture() -> FixtureState {
        let state = FixtureState::default();
        state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<!doctype html><html><head><title>JS Docs</title></head><body><main>
<h1>JS Docs</h1><p>Index whose navigation is rendered by script.</p><nav id="sidebar"></nav>
</main><script>
setTimeout(function () {
  var toc = [{ title: "Alpha", url: "a" }, { title: "Beta", url: "b" }];
  var nav = document.getElementById("sidebar");
  toc.forEach(function (item) {
    var link = document.createElement("a");
    link.href = "/docs/" + item.url;
    link.textContent = item.title;
    nav.appendChild(link);
  });
}, 300);
</script></body></html>"#,
        );
        for (path, title) in [("/docs/a", "Alpha"), ("/docs/b", "Beta")] {
            state.set_html(
                path,
                StatusCode::OK,
                &format!(
                    "<html><head><title>{title}</title></head><body><main><h1>{title}</h1><p>{title} page reached through a script-built link.</p></main></body></html>"
                ),
            );
        }
        state
    }

    async fn add_js_links_source(render_js: bool) -> crate::ingest::SyncSummary {
        let (addr, server_handle) = spawn_fixture_server(js_links_fixture()).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let Added { summary, .. } = SourceLifecycle::new(cfg)
            .add(
                AddSourceParams {
                    origin: format!("http://{addr}/docs/"),
                    name: Some("js-links".to_string()),
                    render_js,
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add js-links source");
        server_handle.abort();
        summary.expect("summary")
    }

    #[tokio::test]
    async fn static_crawl_cannot_see_script_built_links() {
        let summary = add_js_links_source(false).await;
        assert_eq!(
            summary.pages_added, 1,
            "only the index is reachable without JS"
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_crawl_follows_script_built_links() {
        let summary = add_js_links_source(true).await;
        assert_eq!(
            summary.pages_added, 3,
            "render_js must wait for script-built links; warnings: {:?}",
            summary.warnings
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_render_js_syncs_each_get_their_own_browser() {
        let (first, second) = tokio::join!(add_js_links_source(true), add_js_links_source(true));
        assert_eq!(first.pages_added, 3, "first sync: {:?}", first.warnings);
        assert_eq!(second.pages_added, 3, "second sync: {:?}", second.warnings);
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_without_reachable_browser_is_an_actionable_error() {
        use super::{configure_website, ensure_render_browser, WebScope};
        use url::Url;

        let origin = Url::parse("https://example.com/docs/").expect("url");
        let scope = WebScope::new(&origin, &[], &[]).expect("scope");
        let mut website = configure_website(&origin, &scope, 5, 1, true);
        website.with_chrome_connection(Some("http://127.0.0.1:9".to_string()));

        let err = ensure_render_browser(&website)
            .await
            .expect_err("an unreachable browser must fail the Sync, not degrade silently");
        let message = err.to_string();
        assert!(
            message.contains("Chrome") && message.contains("CHROME_URL"),
            "error must say how to fix it, got: {message}"
        );
    }

    #[tokio::test]
    async fn seeds_from_sitemap_respects_robots_and_filters_patterns() {
        let state = FixtureState::default();
        let (addr, server_handle) = spawn_fixture_server(state.clone()).await;

        state.set_text(
            "/robots.txt",
            StatusCode::OK,
            "User-agent: *\nDisallow: /docs/private\n",
        );
        state.set_xml(
            "/sitemap.xml",
            StatusCode::OK,
            &format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\
                   <url><loc>http://{addr}/docs/from-sitemap</loc></url>\
                   <url><loc>http://{addr}/docs/private/secret</loc></url>\
                   <url><loc>http://{addr}/docs/drafts/wip</loc></url>\
                   <url><loc>http://{addr}/blog/news</loc></url>\
                 </urlset>"
            ),
        );
        state.set_html(
            "/docs/",
            StatusCode::OK,
            "<html><head><title>Home</title></head><body><main><h1>Home</h1><p>Docs landing page.</p></main></body></html>",
        );
        state.set_html(
            "/docs/from-sitemap",
            StatusCode::OK,
            "<html><head><title>Sitemap Page</title></head><body><main><h1>Sitemap Page</h1><p>Discovered via sitemap.xml tokensitemap.</p></main></body></html>",
        );
        state.set_html(
            "/docs/private/secret",
            StatusCode::OK,
            "<html><body><h1>Private</h1><p>Blocked by robots.txt.</p></body></html>",
        );
        state.set_html(
            "/docs/drafts/wip",
            StatusCode::OK,
            "<html><body><h1>WIP</h1><p>Excluded by pattern.</p></body></html>",
        );
        state.set_html(
            "/blog/news",
            StatusCode::OK,
            "<html><body><h1>Blog</h1><p>Outside prefix.</p></body></html>",
        );

        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let origin = format!("http://{addr}/docs/");

        let Added {
            source,
            summary: summary_opt,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin,
                    name: Some("sitemap-docs".to_string()),
                    exclude_patterns: vec!["docs/drafts/*".to_string()],
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("sitemap add");
        server_handle.abort();

        let summary = summary_opt.expect("summary");
        assert_eq!(summary.pages_added, 2);

        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");
        let uris: Vec<String> = conn
            .prepare("SELECT uri FROM pages WHERE source_id = ?1 ORDER BY uri")
            .expect("prepare")
            .query_map([source.id], |r| r.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(
            uris,
            vec![
                format!("http://{addr}/docs/"),
                format!("http://{addr}/docs/from-sitemap"),
            ]
        );

        let (etag, last_mod): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT etag, last_modified FROM pages WHERE source_id = ?1 AND uri = ?2",
                rusqlite::params![source.id, format!("http://{addr}/docs/from-sitemap")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query validators");
        assert_eq!(etag.as_deref(), Some("\"etag-/docs/from-sitemap\""));
        assert_eq!(last_mod.as_deref(), Some("Wed, 21 Oct 2026 07:28:00 GMT"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn honors_max_pages_and_include_override() {
        let state = FixtureState::default();
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><body><nav><a href="/docs/p1">P1</a><a href="/docs/p2">P2</a><a href="/blog/included">Included</a><a href="/blog/ignored">Ignored</a></nav><main><h1>Docs</h1><p>Root page.</p></main></body></html>"#,
        );
        state.set_html(
            "/docs/p1",
            StatusCode::OK,
            "<html><body><main><h1>Page 1</h1><p>First page.</p></main></body></html>",
        );
        state.set_html(
            "/docs/p2",
            StatusCode::OK,
            "<html><body><main><h1>Page 2</h1><p>Second page.</p></main></body></html>",
        );
        state.set_html(
            "/blog/included",
            StatusCode::OK,
            "<html><body><main><h1>Included Blog</h1><p>Explicitly included outside /docs/.</p></main></body></html>",
        );
        state.set_html(
            "/blog/ignored",
            StatusCode::OK,
            "<html><body><main><h1>Ignored Blog</h1><p>Not included.</p></main></body></html>",
        );

        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();

        // 1. Test include_patterns overriding /docs/ prefix to include /blog/included while ignoring /blog/ignored.
        let origin = format!("http://{addr}/docs/");
        let Added {
            source: inc_source,
            summary: inc_sum,
        } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: origin.clone(),
                    name: Some("include-override".to_string()),
                    include_patterns: vec!["docs/*".to_string(), "blog/included*".to_string()],
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("include override crawl");
        assert_eq!(inc_sum.expect("inc summary").pages_added, 4);

        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");
        let inc_uris: Vec<String> = conn
            .prepare("SELECT uri FROM pages WHERE source_id = ?1 ORDER BY uri")
            .expect("prepare")
            .query_map([inc_source.id], |r| r.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert!(inc_uris.contains(&format!("http://{addr}/blog/included")));
        assert!(!inc_uris.iter().any(|u| u.contains("/blog/ignored")));

        // 2. Test the max_pages cap.
        let vault_dir2 = tempdir().expect("vault tempdir 2");
        let cfg2 = VaultConfig::new(vault_dir2.path()).allowing_private_origins();
        let lifecycle2 = SourceLifecycle::new(cfg2.clone());
        let Added {
            source,
            summary: summary_opt,
        } = lifecycle2
            .add(
                AddSourceParams {
                    origin,
                    name: Some("capped-docs".to_string()),
                    max_pages: Some(2),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("capped add");

        let summary = summary_opt.expect("summary");
        assert_eq!(
            summary.pages_added, 2,
            "must cap at max_pages=2: {summary:?}"
        );

        let resync = lifecycle2
            .sync(&source.name, false)
            .await
            .expect("resync capped source")
            .synced()
            .expect("synced");
        // Which 2 of the 3 linked pages a capped crawl visits is not deterministic.
        assert_eq!(resync.pages_added + resync.pages_unchanged, 2);
        assert_eq!(resync.pages_stale, 0);

        // 3. Pages a capped crawl (max_pages=2) does not revisit must NOT be marked stale.
        let conn2 = open_rw_db(&cfg2.db_path()).expect("open_rw_db cfg2");
        conn2
            .execute(
                "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
                 VALUES (?1, ?2, 'Archived', '# Archived\n\nOld page.\n', 'hash-old', 3, 'active')",
                rusqlite::params![source.id, format!("http://{addr}/docs/archived")],
            )
            .expect("insert extra active page outside cap");
        drop(conn2);

        let recapped = lifecycle2
            .sync(&source.name, false)
            .await
            .expect("recrawl capped source")
            .synced()
            .expect("synced");
        server_handle.abort();
        assert_eq!(
            recapped.pages_stale, 0,
            "capped crawl must not mark unvisited pages beyond max_pages as stale"
        );
    }
}
