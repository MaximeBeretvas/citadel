use std::collections::{HashMap, HashSet};
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
/// Run on every rendered page (path `/` matches all) after it settles and before its HTML is
/// read. Two steps:
/// 1. Web components draw into shadow DOM, which a page's HTML does not include (DevNet's API
///    reference renders each endpoint this way), so copy every open shadow root, innermost first,
///    into a plain element under its host. Styles and scripts are dropped.
/// 2. Drop navigation: a list or nav block with 30+ links whose text is over 80% link text is a
///    menu (a script-built sidebar can outweigh the page body a hundred to one). It is wrapped in
///    a `<nav>`, which the Markdown conversion ignores, rather than removed, because the crawler
///    reads this same HTML for links to follow. It is dropped only when at least 200 characters
///    of other text remain, so an index page keeps its list.
const PREPARE_RENDERED_PAGE_JS: &str = r#"(function () {
  (function flatten(root) {
    root.querySelectorAll('*').forEach(function (el) {
      if (!el.shadowRoot) { return; }
      flatten(el.shadowRoot);
      var copy = document.createElement('div');
      copy.setAttribute('data-shadow-content', '');
      copy.innerHTML = el.shadowRoot.innerHTML;
      copy.querySelectorAll('style, script, link, template').forEach(function (n) { n.remove(); });
      el.appendChild(copy);
    });
  })(document);

  if (!document.body) { return; }
  var size = function (el) { return (el.textContent || '').trim().length; };
  var menus = [];
  document.body.querySelectorAll('nav, aside, ul, ol').forEach(function (el) {
    if (menus.some(function (menu) { return menu.contains(el); })) { return; }
    var links = el.querySelectorAll('a');
    var total = size(el);
    if (links.length < 30 || total === 0) { return; }
    var linked = 0;
    links.forEach(function (a) { linked += size(a); });
    if (linked / total > 0.8) { menus.push(el); }
  });
  var pageText = document.body.cloneNode(true);
  pageText.querySelectorAll('script, style').forEach(function (n) { n.remove(); });
  var remaining = size(pageText);
  menus.forEach(function (menu) { remaining -= size(menu); });
  if (remaining < 200) { return; }
  menus.forEach(function (menu) {
    if (menu.tagName === 'NAV' || !menu.parentNode) { return; }
    var wrapper = document.createElement('nav');
    wrapper.setAttribute('data-citadel-menu', '');
    menu.parentNode.insertBefore(wrapper, menu);
    wrapper.appendChild(menu);
  });
})();"#;
/// A `render_js` crawl is stopped when no page finishes for this long (spider's own per-request
/// timeout is 120 s, so a healthy crawl always produces pages well within it).
const RENDER_STALL_LIMIT: Duration = Duration::from_secs(300);
/// Per-page cap on waiting for a JS-rendered page to settle before links are read.
const RENDER_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const SEED_FETCH_TIMEOUT_SECS: u64 = 10;
const MAX_CHILD_SITEMAPS: usize = 10;
const NO_CHROME_WARNING: &str = "render_js is enabled, but this is a lite citadel build without the `chrome` feature (--no-default-features); fell back to static HTTP crawl";

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

/// Counts of one web Sync, logged as one `web_sync_summary` line when the Sync ends.
#[derive(Debug, Default)]
pub(crate) struct WebSyncAccounting {
    /// The delay spider reports after the first crawl (`Crawl-delay` in `robots.txt` can change it).
    pub delay_ms: Option<u64>,
    /// Every page the receiving task got from any crawler.
    pub pages_received: usize,
    /// Pages spider delivered that the receiving task missed because it fell behind.
    pub lost_by_listener: usize,
    /// Pages the first crawl delivered and lost; with them, `is_cut_off` sees whether that crawl
    /// used up spider's limit. Not printed.
    pub first_crawl_received: usize,
    pub first_crawl_lost: usize,
    pub later_crawls_started: usize,
    /// Distinct storable URLs among the collected pages after the later crawls. Not printed.
    pub collected_storable_urls: usize,
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
    /// The requested URLs of the first pages dropped as `redirected_out_of_scope`. Not printed.
    pub redirect_examples: Vec<String>,
    pub cut_off: bool,
    pub discovery_ms: Option<u64>,
    pub first_crawl_ms: Option<u64>,
    pub later_crawls_ms: Option<u64>,
    pub conversion_ms: Option<u64>,
    pub write_ms: Option<u64>,
}

/// A step's duration on the summary line: milliseconds, or `-` for a step the Sync never reached.
struct StepMs(Option<u64>);

impl std::fmt::Display for StepMs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("-"),
        }
    }
}

/// Whether a Sync stopped because it used up its page budget, so pages it did not visit must be
/// kept. Any one condition is enough:
/// 1. the first crawl delivered or lost `max_pages` pages: spider's limit ran out (a single
///    crawler cannot fetch a page twice);
/// 2. the distinct storable URLs among the collected pages reach `max_pages`;
/// 3. the results (stored or failed) reach `max_pages`.
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
fn budget_warning(accounting: &WebSyncAccounting, max_pages: usize) -> Option<String> {
    let redirected = accounting.redirected_out_of_scope;
    let received = accounting.pages_received;
    if !accounting.cut_off
        || redirected < MIN_REDIRECTS_FOR_WARNING
        || redirected * 100 <= received * REDIRECT_SHARE_PERCENT
    {
        return None;
    }
    let examples = accounting.redirect_examples.join(", ");
    Some(format!(
        "{redirected} of {received} pages received redirected outside this Source and were not \
         stored, for example {examples}. They count against max_pages ({max_pages}), so other \
         pages were not reached. Raise max_pages, or exclude that section if you do not need it."
    ))
}

/// Record how long a step took and pass its value on unchanged. Applied to a step's `Result`
/// before `?`, so a step that ran and failed still has a duration and its summary field is not `-`.
fn finish_step<T>(slot: &mut Option<u64>, start: std::time::Instant, value: T) -> T {
    *slot = Some(millis(start.elapsed()));
    value
}

/// A duration in whole milliseconds, saturating at `u64::MAX`.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Synchronize a web documentation source using `spider` and `spider_transformations`.
pub async fn sync_web_source(
    db_path: &Path,
    source: &SourceRecord,
    prune: bool,
) -> Result<SyncSummary> {
    sync_web_source_with_accounting(db_path, source, prune)
        .await
        .0
}

/// Run a web Sync and return its accounting record with the result. Every Sync whose Origin
/// passes the checks logs one `web_sync_summary` line, whether it succeeds or fails.
pub(crate) async fn sync_web_source_with_accounting(
    db_path: &Path,
    source: &SourceRecord,
    prune: bool,
) -> (Result<SyncSummary>, WebSyncAccounting) {
    let mut acc = WebSyncAccounting::default();
    let (origin, scope) = match web_origin_and_scope(source) {
        Ok(checked) => checked,
        Err(err) => return (Err(err), acc),
    };
    let result = sync_checked_web_source(db_path, source, prune, &origin, &scope, &mut acc).await;
    log_web_sync_summary(source, result.is_ok(), &acc);
    (result, acc)
}

fn log_web_sync_summary(source: &SourceRecord, ok: bool, acc: &WebSyncAccounting) {
    tracing::info!(
        source = ?source.name,
        origin = ?source.origin,
        ok,
        max_pages = source.max_pages,
        cut_off = acc.cut_off,
        delay_ms = %StepMs(acc.delay_ms),
        pages_received = acc.pages_received,
        results_stored = acc.results_stored,
        results_failed = acc.results_failed,
        lost_by_listener = acc.lost_by_listener,
        unparsable_url = acc.unparsable_url,
        already_collected = acc.already_collected,
        redirected_out_of_scope = acc.redirected_out_of_scope,
        url_not_storable = acc.url_not_storable,
        duplicate_result = acc.duplicate_result,
        markdown_twin = acc.markdown_twin,
        canonical_duplicate = acc.canonical_duplicate,
        binary_file = acc.binary_file,
        later_crawls_started = acc.later_crawls_started,
        discovery_ms = %StepMs(acc.discovery_ms),
        first_crawl_ms = %StepMs(acc.first_crawl_ms),
        later_crawls_ms = %StepMs(acc.later_crawls_ms),
        conversion_ms = %StepMs(acc.conversion_ms),
        write_ms = %StepMs(acc.write_ms),
        "web_sync_summary"
    );
}

fn web_origin_and_scope(source: &SourceRecord) -> Result<(Url, WebScope)> {
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
    Ok((origin, scope))
}

async fn sync_checked_web_source(
    db_path: &Path,
    source: &SourceRecord,
    prune: bool,
    origin: &Url,
    scope: &WebScope,
    acc: &mut WebSyncAccounting,
) -> Result<SyncSummary> {
    let run_id = {
        let conn = open_rw_db(db_path)?;
        start_sync_run(&conn, source.id)?
    };

    let crawl_res = execute_web_crawl(source, origin, scope, acc).await;

    let mut conn = open_rw_db(db_path)?;
    match crawl_res {
        Ok((outcomes, mut warnings)) => {
            let cut_off = is_cut_off(
                acc.first_crawl_received + acc.first_crawl_lost,
                acc.collected_storable_urls,
                outcomes.len(),
                source.max_pages as usize,
            );
            acc.cut_off = cut_off;
            if let Some(warning) = budget_warning(acc, source.max_pages as usize) {
                warnings.push(warning);
            }
            let missing_policy = if prune {
                MissingPagePolicy::Prune
            } else if cut_off {
                MissingPagePolicy::Preserve
            } else {
                MissingPagePolicy::MarkStale
            };
            let write_start = std::time::Instant::now();
            let persisted = persist_sync_outcomes(
                &mut conn,
                source.id,
                &source.name,
                run_id,
                outcomes,
                missing_policy,
                warnings,
            );
            finish_step(&mut acc.write_ms, write_start, persisted)
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
    acc: &mut WebSyncAccounting,
) -> Result<(Vec<PageIngestOutcome>, Vec<String>)> {
    let mut warnings = Vec::new();
    if source.render_js && !cfg!(feature = "chrome") {
        tracing::warn!(source = %source.name, "{}", NO_CHROME_WARNING);
        warnings.push(NO_CHROME_WARNING.to_string());
    }

    let discovery_start = std::time::Instant::now();
    let seed_urls = finish_step(
        &mut acc.discovery_ms,
        discovery_start,
        discover_seed_urls(origin, scope).await,
    );

    let mut collected_pages: Vec<Page> = Vec::new();
    let mut visited_keys: HashSet<String> = HashSet::new();

    let mut primary = configure_website(
        origin,
        scope,
        source.max_pages,
        source.max_depth,
        source.render_js,
    );
    let first_crawl_start = std::time::Instant::now();
    let (primary_pages, primary_lost) = finish_step(
        &mut acc.first_crawl_ms,
        first_crawl_start,
        run_website_crawl(&mut primary, source.render_js).await,
    )?;
    acc.delay_ms = Some(millis(primary.get_delay()));
    acc.first_crawl_received = primary_pages.len();
    acc.first_crawl_lost = primary_lost;
    acc.pages_received += primary_pages.len();
    acc.lost_by_listener += primary_lost;
    for page in primary_pages {
        if let Some(key) = page_dedup_key(&page) {
            visited_keys.insert(key);
        }
        collected_pages.push(page);
    }

    let later_crawls_start = std::time::Instant::now();
    let later_crawls = run_later_crawls(
        source,
        scope,
        seed_urls,
        &mut collected_pages,
        &mut visited_keys,
        acc,
    )
    .await;
    finish_step(&mut acc.later_crawls_ms, later_crawls_start, later_crawls)?;
    acc.collected_storable_urls = count_scoped_urls(&collected_pages, scope);

    if source.render_js && cfg!(feature = "chrome") {
        confirm_render_failures(&mut collected_pages, origin).await;
    }

    let conversion_start = std::time::Instant::now();
    let converted = build_page_outcomes(collected_pages, scope, acc);
    let (outcomes, ok_count, transport_failures) =
        finish_step(&mut acc.conversion_ms, conversion_start, converted);

    let results = result_count(&outcomes);
    if ok_count == 0 && (results == 0 || transport_failures == results) {
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

/// Results that stand for a page of their own (stored or failed), leaving out superseded copies.
fn result_count(outcomes: &[PageIngestOutcome]) -> usize {
    outcomes
        .iter()
        .filter(|o| !matches!(o, PageIngestOutcome::Retired { .. }))
        .count()
}

/// Start one more crawler per discovered URL that no earlier crawl collected, one after another,
/// each limited to what is left of the page budget.
async fn run_later_crawls(
    source: &SourceRecord,
    scope: &WebScope,
    seed_urls: Vec<Url>,
    collected_pages: &mut Vec<Page>,
    visited_keys: &mut HashSet<String>,
    acc: &mut WebSyncAccounting,
) -> Result<()> {
    for seed in seed_urls {
        let seed_key = canonical_dedup_key(&seed);
        if visited_keys.contains(&seed_key) {
            continue;
        }
        let used_budget = count_scoped_documents(collected_pages, scope);
        if used_budget >= source.max_pages as usize {
            break;
        }

        let remaining = source.max_pages.saturating_sub(used_budget as u32);
        if remaining == 0 {
            break;
        }

        let mut seed_site =
            configure_website(&seed, scope, remaining, source.max_depth, source.render_js);
        acc.later_crawls_started += 1;
        let (extra_pages, extra_lost) = run_website_crawl(&mut seed_site, source.render_js).await?;
        acc.pages_received += extra_pages.len();
        acc.lost_by_listener += extra_lost;
        for page in extra_pages {
            let Some(key) = page_dedup_key(&page) else {
                acc.unparsable_url += 1;
                continue;
            };
            if visited_keys.insert(key) {
                collected_pages.push(page);
            } else {
                acc.already_collected += 1;
            }
        }
        visited_keys.insert(seed_key);
    }
    Ok(())
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
            // Citadel seeds from sitemaps itself (`discover_seed_urls`). spider's own sitemap
            // pass reconnects to the browser after the crawl; that pass can hang forever and
            // can report its sitemap 404 against the origin page.
            .with_ignore_sitemap(true)
            .with_execution_scripts(Some(
                [("/".to_string(), PREPARE_RENDERED_PAGE_JS.to_string())]
                    .into_iter()
                    .collect(),
            ))
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

/// When a crawl last produced a page, shared between the page collector and the stall watchdog.
#[derive(Clone)]
struct CrawlActivity {
    started: tokio::time::Instant,
    last_page_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CrawlActivity {
    fn start() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            last_page_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Record that a page just finished.
    fn touch(&self) {
        let elapsed = millis(self.started.elapsed());
        self.last_page_ms
            .store(elapsed, std::sync::atomic::Ordering::Relaxed);
    }

    /// Resolve once no page has finished (and the crawl has not started one) for `limit`.
    async fn stalled_for(&self, limit: Duration) {
        loop {
            let last =
                Duration::from_millis(self.last_page_ms.load(std::sync::atomic::Ordering::Relaxed));
            let idle = self.started.elapsed().saturating_sub(last);
            if idle >= limit {
                return;
            }
            tokio::time::sleep(limit - idle).await;
        }
    }
}

/// Run one crawler and return the pages it delivered with the number the receiving task missed
/// because it fell behind.
async fn run_website_crawl(website: &mut Website, render_js: bool) -> Result<(Vec<Page>, usize)> {
    #[cfg(feature = "chrome")]
    let render_browser = if render_js {
        prepare_render_browser(website).await?
    } else {
        None
    };

    let mut rx = website.subscribe(256);
    let activity = CrawlActivity::start();
    let collector_activity = activity.clone();
    let collector = tokio::spawn(async move {
        let mut pages = Vec::new();
        let mut lost = 0usize;
        loop {
            match rx.recv().await {
                Ok(page) => {
                    collector_activity.touch();
                    pages.push(page);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "spider broadcast channel lagged during crawl");
                    lost = lost.saturating_add(usize::try_from(skipped).unwrap_or(usize::MAX));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        (pages, lost)
    });

    let mut stalled = false;
    if render_js && cfg!(feature = "chrome") {
        // A browser crawl can wait forever on a page or connection that never answers. Stop it
        // once no page has finished for `RENDER_STALL_LIMIT` instead of hanging the Sync.
        tokio::select! {
            () = website.crawl() => {}
            () = activity.stalled_for(RENDER_STALL_LIMIT) => stalled = true,
        }
    } else {
        website.crawl_raw().await;
    }
    website.unsubscribe();

    #[cfg(feature = "chrome")]
    if let Some(browser) = render_browser {
        browser.shutdown().await;
    }

    if stalled {
        collector.abort();
        tracing::warn!(url = %website.get_url(), "render_js crawl stalled; Sync stopped");
        return Err(VaultError::Other(format!(
            "render_js crawl stalled: no page finished for {} minutes, so the Sync was stopped \
             and the previously stored pages were kept",
            RENDER_STALL_LIMIT.as_secs() / 60
        )));
    }

    collector
        .await
        .map_err(|err| VaultError::Other(format!("crawler collector task panicked: {err}")))
}

/// Re-check rendered pages that the browser reported as HTTP 4xx. A browser crawl occasionally
/// reports another response's status for a page (seen: the 404 of the browser's own
/// `/favicon.ico` request recorded against a page served with 200), so a client error is only
/// kept when a direct request to the server agrees.
async fn confirm_render_failures(pages: &mut [Page], origin: &Url) {
    let suspects = pages.iter().any(is_unconfirmed_client_error);
    if !suspects {
        return;
    }
    let client = match build_seed_http_client(origin) {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build HTTP client to confirm page errors");
            return;
        }
    };
    for page in pages
        .iter_mut()
        .filter(|page| is_unconfirmed_client_error(page))
    {
        let server_status = match client.get(page.get_url()).send().await {
            Ok(response) => Some(response.status()),
            Err(_) => None,
        };
        let confirmed = confirmed_status(page.status_code, server_status);
        if confirmed != page.status_code {
            tracing::debug!(
                url = page.get_url(),
                browser_status = page.status_code.as_u16(),
                server_status = confirmed.as_u16(),
                "browser-reported page error not confirmed by the server"
            );
            page.status_code = confirmed;
        }
    }
}

/// A rendered page with content whose only problem is a browser-reported 4xx status.
fn is_unconfirmed_client_error(page: &Page) -> bool {
    page.status_code.is_client_error()
        && page.error_status.is_none()
        && !page.get_html_bytes_u8().is_empty()
}

/// The status to record for a page the browser reported as a client error: the server's own
/// answer when it says the page is fine, otherwise the browser's.
fn confirmed_status(
    browser: reqwest::StatusCode,
    server: Option<reqwest::StatusCode>,
) -> reqwest::StatusCode {
    match server {
        Some(status) if status.is_success() => status,
        _ => browser,
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

/// Turn collected pages into results, counting each page that gets none in `acc` under the first
/// reason it meets. Copies of one document (see [`pair_documents`]) become one result at the
/// document's URI plus a `Retired` result per copy.
fn build_page_outcomes(
    pages: Vec<Page>,
    scope: &WebScope,
    acc: &mut WebSyncAccounting,
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
            acc.unparsable_url += 1;
            continue;
        };

        if redirected_out_of_scope(page, scope) {
            acc.redirected_out_of_scope += 1;
            if acc.redirect_examples.len() < MAX_REDIRECT_EXAMPLES {
                acc.redirect_examples.push(page.get_url().to_string());
            }
            continue;
        }

        if !scope.should_store(&canonical_url) {
            acc.url_not_storable += 1;
            continue;
        }

        let uri = canonical_url.to_string();
        if !seen_uris.insert(uri.clone()) {
            acc.duplicate_result += 1;
            continue;
        }

        if let Some(kept) = superseded.get(&index) {
            match kept {
                Kept::MarkdownTwinOf(_) => acc.markdown_twin += 1,
                Kept::CanonicalOf(_) => acc.canonical_duplicate += 1,
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
            acc.results_failed += 1;
            continue;
        }

        if page.binary_file {
            acc.binary_file += 1;
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
        acc.results_stored += 1;
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
        .user_agent(USER_AGENT)
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
    use std::time::Duration;
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
        /// Every request served, as (path, status, user-agent), for diagnosing failures.
        requests: Arc<RwLock<Vec<(String, u16, String)>>>,
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

        fn set_markdown(&self, path: &str, status: StatusCode, body: &str) {
            let mut guard = self.routes.write().expect("lock routes");
            guard.insert(
                path.to_string(),
                (
                    status,
                    "text/markdown; charset=utf-8".to_string(),
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
        headers: axum::http::HeaderMap,
    ) -> Response {
        let path = uri.path().to_string();
        let entry = {
            let guard = state.routes.read().expect("lock routes");
            guard.get(&path).cloned()
        };
        let agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .chars()
            .take(40)
            .collect::<String>();
        state.requests.write().expect("lock requests").push((
            uri.to_string(),
            entry.as_ref().map_or(404, |(status, _, _)| status.as_u16()),
            agent,
        ));
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
        #[cfg(feature = "chrome")]
        let _chrome = CHROME_TEST_LOCK.lock().await;
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

    /// Held by every test that launches Chrome, so browsers start one test at a time and do not
    /// starve timing-sensitive tests running in parallel.
    #[cfg(feature = "chrome")]
    static CHROME_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        add_source_on(js_links_fixture(), render_js).await
    }

    async fn add_source_on(fixture: FixtureState, render_js: bool) -> crate::ingest::SyncSummary {
        let (addr, server_handle) = spawn_fixture_server(fixture.clone()).await;
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
        let summary = summary.expect("summary");
        if summary.pages_failed > 0 {
            let conn = open_rw_db(&vault_dir.path().join("vault.db")).expect("open vault db");
            let mut stmt = conn
                .prepare("SELECT uri, http_status, error_message FROM pages WHERE status = 'error'")
                .expect("prepare");
            let failed: Vec<(String, Option<i64>, Option<String>)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("rows");
            eprintln!("failed pages: {failed:?}");
            eprintln!(
                "fixture requests: {:#?}",
                fixture.requests.read().expect("lock requests")
            );
        }
        summary
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
        let _chrome = CHROME_TEST_LOCK.lock().await;
        let summary = add_js_links_source(true).await;
        assert_eq!(
            summary.pages_added, 3,
            "render_js must wait for script-built links; warnings: {:?}",
            summary.warnings
        );
    }

    #[test]
    fn browser_client_error_is_kept_only_when_the_server_agrees() {
        use super::confirmed_status;
        use reqwest::StatusCode;

        let not_found = StatusCode::NOT_FOUND;
        assert_eq!(
            confirmed_status(not_found, Some(StatusCode::OK)),
            StatusCode::OK
        );
        assert_eq!(confirmed_status(not_found, Some(not_found)), not_found);
        assert_eq!(
            confirmed_status(not_found, Some(StatusCode::FORBIDDEN)),
            not_found
        );
        assert_eq!(confirmed_status(not_found, None), not_found);
    }

    #[tokio::test]
    async fn misreported_page_error_is_corrected_from_the_server() {
        use super::confirm_render_failures;
        use spider::page::build;
        use spider::utils::PageResponse;

        let state = FixtureState::default();
        state.set_html("/docs/", StatusCode::OK, "<html><body>fine</body></html>");
        let (addr, server_handle) = spawn_fixture_server(state).await;
        let origin = url::Url::parse(&format!("http://{addr}/docs/")).expect("origin");

        let page_with = |path: &str| {
            let mut page = build(
                &format!("http://{addr}{path}"),
                PageResponse {
                    content: Some(b"<html><body>rendered</body></html>".to_vec()),
                    ..Default::default()
                },
            );
            page.status_code = reqwest::StatusCode::NOT_FOUND;
            page
        };
        let mut pages = vec![page_with("/docs/"), page_with("/docs/missing")];
        confirm_render_failures(&mut pages, &origin).await;
        server_handle.abort();

        assert_eq!(
            pages[0].status_code,
            reqwest::StatusCode::OK,
            "server says 200"
        );
        assert_eq!(
            pages[1].status_code,
            reqwest::StatusCode::NOT_FOUND,
            "a real 404 stays a 404"
        );
    }

    #[tokio::test]
    async fn stall_watchdog_fires_only_after_pages_stop_arriving() {
        use super::CrawlActivity;

        let activity = CrawlActivity::start();
        let limit = Duration::from_millis(150);
        let keep_alive = activity.clone();
        let pages = tokio::spawn(async move {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                keep_alive.touch();
            }
        });

        let started = std::time::Instant::now();
        activity.stalled_for(limit).await;
        let waited = started.elapsed();
        pages.await.expect("page task");

        // `touch` records whole milliseconds (truncated), so allow 2 ms of rounding.
        assert!(
            waited >= Duration::from_millis(300 + 150 - 2),
            "pages kept arriving for 300 ms, so the stall cannot fire before 450 ms: {waited:?}"
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_stores_content_drawn_inside_shadow_dom() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
        let state = FixtureState::default();
        state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
        // Like DevNet's API reference: the body is rendered by a web component into shadow DOM,
        // nested one level, which a plain HTML read of the page does not include.
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<!doctype html><html><head><title>Shadow Docs</title></head><body>
<h1>Shadow Docs</h1><p>Light text lighttoken.</p><api-ref></api-ref>
<script>
setTimeout(function () {
  var outer = document.querySelector("api-ref").attachShadow({ mode: "open" });
  outer.innerHTML = "<style>p{color:red}</style><h2>Endpoint</h2><p>Outer body outertoken.</p><api-params></api-params>";
  var inner = outer.querySelector("api-params").attachShadow({ mode: "open" });
  inner.innerHTML = "<p>Nested parameter innertoken.</p>";
}, 200);
</script></body></html>"#,
        );
        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: format!("http://{addr}/docs/"),
                    name: Some("shadow".to_string()),
                    render_js: true,
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add shadow source");
        server_handle.abort();

        let conn = open_rw_db(&cfg.db_path()).expect("open vault db");
        let content: String = conn
            .query_row("SELECT content_md FROM pages LIMIT 1", [], |row| row.get(0))
            .expect("stored page");
        for token in ["outertoken", "innertoken"] {
            assert!(content.contains(token), "missing {token} in: {content}");
        }
        assert!(
            !content.contains("color:red"),
            "styles must not be stored: {content}"
        );
    }

    /// Sync a one-page `render_js` Source serving `body` and return the stored Markdown.
    #[cfg(feature = "chrome")]
    async fn rendered_markdown(body: &str) -> String {
        let state = FixtureState::default();
        state.set_text(
            "/robots.txt",
            StatusCode::OK,
            "User-agent: *\nDisallow: /docs/x\n",
        );
        state.set_html("/docs/", StatusCode::OK, body);
        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: format!("http://{addr}/docs/"),
                    name: Some("rendered".to_string()),
                    render_js: true,
                    max_pages: Some(1),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add rendered source");
        server_handle.abort();
        let conn = open_rw_db(&cfg.db_path()).expect("open vault db");
        conn.query_row(
            "SELECT content_md FROM pages WHERE uri LIKE '%/docs/'",
            [],
            |row| row.get(0),
        )
        .expect("stored page")
    }

    #[cfg(feature = "chrome")]
    fn link_list(count: usize) -> String {
        (0..count)
            .map(|i| format!(r#"<li><a href="/docs/x{i}">Menu entry navtoken {i}</a></li>"#))
            .collect()
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_drops_a_big_navigation_list_next_to_real_content() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
        let body = format!(
            "<html><head><title>Endpoint</title></head><body><div><ul>{}</ul></div>\
             <div><h1>Get Things</h1><p>{}</p></div></body></html>",
            link_list(60),
            "Returns the things bodytoken with enough explanation to count as content. ".repeat(6)
        );
        let content = rendered_markdown(&body).await;
        assert!(content.contains("bodytoken"), "{content}");
        assert!(
            !content.contains("navtoken"),
            "navigation must be dropped: {content}"
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_keeps_a_link_list_that_is_the_page_content() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
        let body = format!(
            "<html><head><title>Index</title></head><body><h1>Index</h1><ul>{}</ul></body></html>",
            link_list(60)
        );
        let content = rendered_markdown(&body).await;
        assert!(
            content.contains("navtoken 59"),
            "an index page keeps its list: {content}"
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_follows_links_of_a_dropped_navigation_list() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
        let state = FixtureState::default();
        state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
        // A script-built sidebar big enough to count as a menu, next to real page content: it
        // is left out of the stored Markdown but its links must still be crawled.
        state.set_html(
            "/docs/",
            StatusCode::OK,
            &format!(
                r#"<!doctype html><html><head><title>Menu Docs</title></head><body>
<div id="sidebar"></div><main><h1>Menu Docs</h1><p>{}</p></main><script>
setTimeout(function () {{
  var list = document.createElement("ul");
  for (var i = 0; i < 40; i++) {{
    var item = document.createElement("li");
    var link = document.createElement("a");
    link.href = "/docs/p" + i;
    link.textContent = "Menu entry navtoken " + i;
    item.appendChild(link);
    list.appendChild(item);
  }}
  document.getElementById("sidebar").appendChild(list);
}}, 300);
</script></body></html>"#,
                "Body text bodytoken with enough explanation to count as content. ".repeat(6)
            ),
        );
        for i in 0..40 {
            state.set_html(
                &format!("/docs/p{i}"),
                StatusCode::OK,
                &format!(
                    "<html><head><title>P{i}</title></head><body><main><h1>P{i}</h1><p>Page {i} reached through the sidebar.</p></main></body></html>"
                ),
            );
        }
        let (addr, server_handle) = spawn_fixture_server(state).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let Added { summary, .. } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: format!("http://{addr}/docs/"),
                    name: Some("menu".to_string()),
                    render_js: true,
                    max_pages: Some(3),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add menu source");
        server_handle.abort();
        let summary = summary.expect("summary");
        assert_eq!(
            summary.pages_added, 3,
            "sidebar links must still be followed; warnings: {:?}",
            summary.warnings
        );

        let conn = open_rw_db(&cfg.db_path()).expect("open vault db");
        let index: String = conn
            .query_row(
                "SELECT content_md FROM pages WHERE uri LIKE '%/docs/'",
                [],
                |row| row.get(0),
            )
            .expect("stored index");
        assert!(index.contains("bodytoken"), "{index}");
        assert!(
            !index.contains("navtoken"),
            "the menu must stay out of the Markdown: {index}"
        );
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_render_js_syncs_each_get_their_own_browser() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
        crate::init_tracing();
        let (first, second) = tokio::join!(add_js_links_source(true), add_js_links_source(true));
        assert_eq!(first.pages_added, 3, "first sync: {first:?}");
        assert_eq!(second.pages_added, 3, "second sync: {second:?}");
    }

    #[cfg(feature = "chrome")]
    #[tokio::test(flavor = "multi_thread")]
    async fn render_js_without_reachable_browser_is_an_actionable_error() {
        let _chrome = CHROME_TEST_LOCK.lock().await;
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

    fn hand_made_page(url: &str) -> super::Page {
        let mut page = super::Page::default();
        page.set_url(url.to_string());
        page.status_code = StatusCode::OK;
        page
    }

    #[test]
    fn dropped_pages_are_counted_by_reason() {
        use super::{build_page_outcomes, Url, WebScope, WebSyncAccounting};

        let origin = Url::parse("https://docs.example/docs/").expect("origin");
        let scope = WebScope::new(&origin, &[], &[]).expect("scope");

        let mut redirected = hand_made_page("https://docs.example/docs/moved");
        redirected.final_redirect_destination = Some("https://elsewhere.example/x".to_string());
        let mut binary = hand_made_page("https://docs.example/docs/file");
        binary.binary_file = true;
        let mut missing = hand_made_page("https://docs.example/docs/missing");
        missing.status_code = StatusCode::NOT_FOUND;

        let pages = vec![
            hand_made_page("ftp://docs.example/docs/unparsable"),
            redirected,
            hand_made_page("https://docs.example/blog/outside"),
            hand_made_page("https://docs.example/docs/stored"),
            hand_made_page("https://docs.example/docs/stored"),
            binary,
            missing,
        ];

        let mut acc = WebSyncAccounting::default();
        let (outcomes, ok_count, _) = build_page_outcomes(pages, &scope, &mut acc);

        assert_eq!(outcomes.len(), 2);
        assert_eq!(ok_count, 1);
        assert_eq!(acc.unparsable_url, 1);
        assert_eq!(acc.redirected_out_of_scope, 1);
        assert_eq!(acc.url_not_storable, 1);
        assert_eq!(acc.duplicate_result, 1);
        assert_eq!(acc.binary_file, 1);
        assert_eq!(acc.results_stored, 1);
        assert_eq!(acc.results_failed, 1);
    }

    /// A served fixture site with a Source added on `http://{addr}/docs/` but not synced yet.
    struct UnsyncedSite {
        addr: SocketAddr,
        server: tokio::task::JoinHandle<()>,
        _vault_dir: tempfile::TempDir,
        cfg: VaultConfig,
        source: crate::ingest::SourceRecord,
    }

    async fn add_unsynced_docs_source(
        state: &FixtureState,
        params: AddSourceParams,
    ) -> UnsyncedSite {
        let (addr, server) = spawn_fixture_server(state.clone()).await;
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path()).allowing_private_origins();
        let Added { source, .. } = SourceLifecycle::new(cfg.clone())
            .add(
                AddSourceParams {
                    origin: format!("http://{addr}/docs/"),
                    ..params
                },
                false,
            )
            .await
            .expect("add without sync");
        UnsyncedSite {
            addr,
            server,
            _vault_dir: vault_dir,
            cfg,
            source,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accounting_times_every_step_and_counts_later_crawls() {
        use super::sync_web_source_with_accounting;

        let state = FixtureState::default();
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><body><main><h1>Docs</h1><p>Root.</p><a href="/docs/a">A</a><a href="/docs/b">B</a></main></body></html>"#,
        );
        for (path, title) in [("/docs/a", "A"), ("/docs/b", "B")] {
            state.set_html(
                path,
                StatusCode::OK,
                &format!(
                    "<html><body><main><h1>{title}</h1><p>Linked page.</p></main></body></html>"
                ),
            );
        }
        for (path, title) in [("/docs/h1", "Hidden 1"), ("/docs/h2", "Hidden 2")] {
            state.set_html(
                path,
                StatusCode::OK,
                &format!(
                    r#"<html><body><main><h1>{title}</h1><p>Only in llms.txt.</p><a href="/docs/">Home</a></main></body></html>"#
                ),
            );
        }

        let site = add_unsynced_docs_source(
            &state,
            AddSourceParams {
                name: Some("accounted".to_string()),
                ..Default::default()
            },
        )
        .await;
        let addr = site.addr;
        state.set_text(
            "/llms.txt",
            StatusCode::OK,
            &format!(
                "# Docs\n\n- [Hidden 1](http://{addr}/docs/h1)\n- [Hidden 2](http://{addr}/docs/h2)\n"
            ),
        );

        let (result, acc) =
            sync_web_source_with_accounting(&site.cfg.db_path(), &site.source, false).await;
        site.server.abort();
        let summary = result.expect("sync succeeds");
        let requests = state.requests.read().expect("lock requests").clone();

        assert_eq!(acc.later_crawls_started, 2, "{acc:?}\n{requests:?}");
        assert_eq!(acc.already_collected, 6, "{acc:?}\n{requests:?}");
        assert_eq!(acc.pages_received, 11, "{acc:?}\n{requests:?}");
        assert_eq!(acc.lost_by_listener, 0);
        assert_eq!(acc.results_stored, 5, "{summary:?}");
        for (step, value) in [
            ("discovery_ms", acc.discovery_ms),
            ("first_crawl_ms", acc.first_crawl_ms),
            ("later_crawls_ms", acc.later_crawls_ms),
            ("conversion_ms", acc.conversion_ms),
            ("write_ms", acc.write_ms),
        ] {
            assert!(value.is_some(), "{step} not set: {acc:?}");
        }
        assert_eq!(
            acc.pages_received,
            acc.results_stored
                + acc.results_failed
                + acc.unparsable_url
                + acc.already_collected
                + acc.redirected_out_of_scope
                + acc.url_not_storable
                + acc.duplicate_result
                + acc.markdown_twin
                + acc.canonical_duplicate
                + acc.binary_file,
            "{acc:?}"
        );
    }

    fn redirect_accounting(
        redirected: usize,
        received: usize,
        cut_off: bool,
    ) -> super::WebSyncAccounting {
        super::WebSyncAccounting {
            redirected_out_of_scope: redirected,
            pages_received: received,
            cut_off,
            redirect_examples: vec![
                "https://docs.example/docs/r1".to_string(),
                "https://docs.example/docs/r2".to_string(),
                "https://docs.example/docs/r3".to_string(),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn warns_when_budget_went_to_redirects() {
        let warning =
            super::budget_warning(&redirect_accounting(5, 20, true), 20).expect("a warning");
        assert!(warning.starts_with("5 of 20 pages received redirected outside this Source"));
        for example in ["/docs/r1", "/docs/r2", "/docs/r3"] {
            assert!(warning.contains(example), "{warning}");
        }
        assert!(warning.contains("max_pages (20)"), "{warning}");
    }

    #[test]
    fn no_warning_below_five_redirects() {
        assert_eq!(
            super::budget_warning(&redirect_accounting(4, 20, true), 20),
            None
        );
    }

    #[test]
    fn no_warning_at_ten_percent() {
        assert_eq!(
            super::budget_warning(&redirect_accounting(5, 50, true), 50),
            None
        );
    }

    #[test]
    fn warns_above_ten_percent() {
        assert!(super::budget_warning(&redirect_accounting(5, 49, true), 49).is_some());
    }

    #[test]
    fn no_warning_when_not_cut_off() {
        assert_eq!(
            super::budget_warning(&redirect_accounting(5, 20, false), 20),
            None
        );
    }

    #[test]
    fn failed_step_keeps_its_duration() {
        let mut slot = None;
        let result: crate::error::Result<()> = super::finish_step(
            &mut slot,
            std::time::Instant::now(),
            Err(crate::error::VaultError::Other("crawl stalled".to_string())),
        );
        assert!(result.is_err());
        assert!(
            slot.is_some(),
            "a step that ran and failed still has a duration"
        );
    }

    #[test]
    fn first_crawl_at_limit_is_cut_off() {
        // The dlthub Sync of 2026-11-23: condition 1 only.
        assert!(super::is_cut_off(1000, 759, 756, 1000));
    }

    #[test]
    fn collected_urls_at_limit_is_cut_off() {
        assert!(super::is_cut_off(40, 100, 90, 100));
    }

    #[test]
    fn todays_condition_still_counts() {
        assert!(super::is_cut_off(2, 2, 3, 3));
    }

    #[test]
    fn under_budget_is_complete() {
        assert!(!super::is_cut_off(40, 60, 60, 100));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cut_off_sync_with_dropped_page_marks_nothing_stale() {
        use super::sync_web_source_with_accounting;

        let state = FixtureState::default();
        state.set_html(
            "/docs/",
            StatusCode::OK,
            r#"<html><body><main><h1>Docs</h1><p>Root.</p><a href="/docs/p1">1</a><a href="/docs/p2">2</a><a href="/docs/p3">3</a><a href="/docs/p4">4</a><a href="/docs/p5">5</a></main></body></html>"#,
        );
        for i in 1..=5 {
            state.set_html(
                &format!("/docs/p{i}"),
                StatusCode::OK,
                &format!(
                    "<html><body><main><h1>Page {i}</h1><p>Body {i}.</p></main></body></html>"
                ),
            );
        }

        let site = add_unsynced_docs_source(
            &state,
            AddSourceParams {
                name: Some("cut-off".to_string()),
                include_patterns: vec!["docs/p*".to_string()],
                max_pages: Some(3),
                ..Default::default()
            },
        )
        .await;
        let cfg = &site.cfg;

        let archived = format!("http://{}/docs/archived", site.addr);
        let conn = open_rw_db(&cfg.db_path()).expect("open_rw_db");
        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
             VALUES (?1, ?2, 'Archived', '# Archived\n\nOld page.\n', 'hash-old', 3, 'active')",
            rusqlite::params![site.source.id, archived],
        )
        .expect("insert archived active page");
        drop(conn);

        let (result, acc) =
            sync_web_source_with_accounting(&cfg.db_path(), &site.source, false).await;
        site.server.abort();
        let summary = result.expect("sync succeeds");
        let requests = state.requests.read().expect("lock requests").clone();

        assert_eq!(acc.url_not_storable, 1, "the Origin is dropped: {acc:?}");
        assert_eq!(acc.results_stored, 2, "{acc:?}\n{requests:?}");
        assert!(acc.cut_off, "{acc:?}");
        assert_eq!(summary.pages_stale, 0, "{summary:?}");
        let status: String = open_rw_db(&cfg.db_path())
            .expect("open_rw_db")
            .query_row(
                "SELECT status FROM pages WHERE uri = ?1",
                [&archived],
                |r| r.get(0),
            )
            .expect("archived row");
        assert_eq!(status, "active");
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

    mod markdown_twins {
        use axum::http::StatusCode;

        use super::{add_unsynced_docs_source, hand_made_page, FixtureState, UnsyncedSite};
        use crate::db::open_rw_db;
        use crate::sources::{AddSourceParams, SourceLifecycle};

        const BANNER: &str = "This is documentation for an unreleased version banner";

        fn html_page(title: &str, alternate: Option<&str>, links: &[&str], body: &str) -> String {
            let alt = alternate
                .map(|href| format!(r#"<link rel="alternate" type="text/markdown" href="{href}">"#))
                .unwrap_or_default();
            let anchors: String = links
                .iter()
                .map(|href| format!(r#"<a href="{href}">{href}</a> "#))
                .collect();
            format!(
                "<!doctype html><html><head><title>{title} | Site</title>{alt}</head><body>\
                 <main><div class=\"banner\">{BANNER}</div><h1>{title}</h1><p>{body}</p>\
                 <p>{anchors}</p></main></body></html>"
            )
        }

        fn markdown_page(title: &str, body: &str) -> String {
            format!("---\ntitle: {title}\ndescription: about {title}\n---\n\n# {title}\n\n{body}\n")
        }

        /// A Docusaurus-like site: every HTML page links to its own `.md` copy, `llms.txt` lists the
        /// `.md` URLs, one `.md` has no HTML page, and one page is also served under `?version=2`.
        fn twin_site() -> FixtureState {
            let state = FixtureState::default();
            state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
            state.set_html(
                "/docs/",
                StatusCode::OK,
                &html_page(
                    "Docs Home",
                    None,
                    &[
                        "/docs/guide",
                        "/docs/api",
                        "/docs/orphan.md",
                        "/docs/plain",
                        "/docs/plain?version=2",
                    ],
                    "Welcome to the documentation hub.",
                ),
            );
            state.set_html(
                "/docs/guide",
                StatusCode::OK,
                &html_page(
                    "User Guide",
                    Some("/docs/guide.md"),
                    &["/docs/guide.md"],
                    "Getting started steps for the guide.",
                ),
            );
            state.set_markdown(
                "/docs/guide.md",
                StatusCode::OK,
                &markdown_page("User Guide", "Getting started steps for the guide."),
            );
            // Minified HTML with unquoted attributes and no link tag: paired by URL suffix only.
            state.set_html(
                "/docs/api",
                StatusCode::OK,
                &format!(
                    "<html><head><title>API | Site</title><link rel=canonical href=/docs/api></head>\
                     <body><main><div>{BANNER}</div><h1>API</h1><p>HTTP endpoints reference.</p>\
                     </main></body></html>"
                ),
            );
            state.set_markdown(
                "/docs/api.md",
                StatusCode::OK,
                &markdown_page("API", "HTTP endpoints reference."),
            );
            state.set_markdown(
                "/docs/orphan.md",
                StatusCode::OK,
                &markdown_page("Orphan", "Only published as Markdown."),
            );
            // Served for both `/docs/plain` and `/docs/plain?version=2` (routes ignore the query).
            state.set_html(
                "/docs/plain",
                StatusCode::OK,
                "<html><head><title>Plain</title><link rel=\"canonical\" href=\"/docs/plain\"></head>\
                 <body><main><h1>Plain</h1><p>A page with no Markdown copy.</p></main></body></html>",
            );
            state.set_text(
                "/docs/llms.txt",
                StatusCode::OK,
                "# Docs\n\n- [User Guide](/docs/guide.md): guide\n- [API](/docs/api.md): api\n",
            );
            state
        }

        async fn add_site(state: FixtureState, max_pages: Option<u32>) -> UnsyncedSite {
            add_unsynced_docs_source(
                &state,
                AddSourceParams {
                    name: Some("twins".to_string()),
                    max_pages,
                    ..Default::default()
                },
            )
            .await
        }

        async fn sync(site: &UnsyncedSite, prune: bool) -> crate::ingest::SyncSummary {
            SourceLifecycle::new(site.cfg.clone())
                .sync(&site.source.name, prune)
                .await
                .expect("sync")
                .synced()
                .expect("synced")
        }

        /// (uri relative to the server, status, title, content_md) of every page, by uri.
        fn pages(site: &UnsyncedSite) -> Vec<(String, String, String, String)> {
            let base = format!("http://{}", site.addr);
            open_rw_db(&site.cfg.db_path())
                .expect("open_rw_db")
                .prepare(
                    "SELECT uri, status, title, content_md FROM pages WHERE source_id = ?1 ORDER BY uri",
                )
                .expect("prepare")
                .query_map([site.source.id], |r| {
                    let uri: String = r.get(0)?;
                    Ok((
                        uri.trim_start_matches(&base).to_string(),
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                    ))
                })
                .expect("query")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("collect")
        }

        fn active_uris(site: &UnsyncedSite) -> Vec<String> {
            pages(site)
                .into_iter()
                .filter(|p| p.1 == "active")
                .map(|p| p.0)
                .collect()
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn stores_one_page_per_markdown_twin() {
            let site = add_site(twin_site(), None).await;
            sync(&site, false).await;
            site.server.abort();

            assert_eq!(
                active_uris(&site),
                vec![
                    "/docs/",
                    "/docs/api",
                    "/docs/guide",
                    "/docs/orphan.md",
                    "/docs/plain"
                ],
                "{:#?}",
                pages(&site)
            );
            for (uri, _, title, md) in pages(&site) {
                if uri == "/docs/guide" || uri == "/docs/api" {
                    assert!(
                        !md.contains(BANNER),
                        "{uri} must hold the .md content: {md}"
                    );
                    assert!(
                        !title.contains("| Site"),
                        "{uri} title from frontmatter: {title}"
                    );
                }
            }
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn resync_marks_stored_markdown_twins_stale() {
            // `Some(7)`: the first crawl reaches all 7 linked URLs, so the Sync is cut off and keeps
            // unvisited pages, yet the merged copies it did visit still go stale.
            for max_pages in [None, Some(7)] {
                let site = add_site(twin_site(), max_pages).await;
                let conn = open_rw_db(&site.cfg.db_path()).expect("open_rw_db");
                conn.execute(
                    "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
                     VALUES (?1, ?2, 'Archived', '# Archived\n', 'hash-old', 1, 'active')",
                    rusqlite::params![site.source.id, format!("http://{}/docs/archived", site.addr)],
                )
                .expect("insert unvisited row");
                for path in ["/docs/guide.md", "/docs/api.md", "/docs/plain?version=2"] {
                    conn.execute(
                        "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
                         VALUES (?1, ?2, 'Old', '# Old\n', 'hash-old', 1, 'active')",
                        rusqlite::params![site.source.id, format!("http://{}{path}", site.addr)],
                    )
                    .expect("insert old twin row");
                }
                drop(conn);

                sync(&site, false).await;
                let after = pages(&site);
                for path in ["/docs/guide.md", "/docs/api.md", "/docs/plain?version=2"] {
                    let row = after.iter().find(|p| p.0 == path);
                    assert_eq!(
                        row.map(|p| p.1.as_str()),
                        Some("stale"),
                        "max_pages={max_pages:?}: {path} must go stale: {after:#?}"
                    );
                }

                let archived = after
                    .iter()
                    .find(|p| p.0 == "/docs/archived")
                    .map(|p| p.1.as_str());
                let expected = if max_pages.is_some() {
                    "active"
                } else {
                    "stale"
                };
                assert_eq!(
                    archived,
                    Some(expected),
                    "max_pages={max_pages:?}: {after:#?}"
                );

                if max_pages.is_none() {
                    sync(&site, true).await;
                    let pruned = pages(&site);
                    assert!(
                        !pruned
                            .iter()
                            .any(|p| p.0.ends_with("guide.md") || p.0.contains('?')),
                        "--prune deletes merged pages: {pruned:#?}"
                    );
                }
                site.server.abort();
            }
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn markdown_only_site_is_crawled_normally() {
            let state = FixtureState::default();
            state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
            state.set_html(
                "/docs/",
                StatusCode::OK,
                &html_page(
                    "Index",
                    None,
                    &["/docs/a.md", "/docs/b.md"],
                    "Markdown files.",
                ),
            );
            state.set_markdown("/docs/a.md", StatusCode::OK, &markdown_page("A", "Alpha."));
            state.set_markdown("/docs/b.md", StatusCode::OK, &markdown_page("B", "Beta."));
            let site = add_site(state, None).await;
            sync(&site, false).await;
            site.server.abort();
            assert_eq!(
                active_uris(&site),
                vec!["/docs/", "/docs/a.md", "/docs/b.md"]
            );
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn failed_twin_falls_back_to_the_other_copy() {
            let state = FixtureState::default();
            state.set_text("/robots.txt", StatusCode::OK, "User-agent: *\nAllow: /\n");
            state.set_html(
                "/docs/",
                StatusCode::OK,
                &html_page(
                    "Index",
                    None,
                    &["/docs/ok-html", "/docs/bad-html.md"],
                    "Two pages.",
                ),
            );
            // HTML fine, .md broken: keep the HTML content, no Error page for the .md.
            state.set_html(
                "/docs/ok-html",
                StatusCode::OK,
                &html_page(
                    "Ok Html",
                    Some("/docs/ok-html.md"),
                    &["/docs/ok-html.md"],
                    "Html body.",
                ),
            );
            state.set_text(
                "/docs/ok-html.md",
                StatusCode::INTERNAL_SERVER_ERROR,
                "boom",
            );
            // HTML broken, .md fine: no pairing, both handled as before.
            state.set_html("/docs/bad-html", StatusCode::NOT_FOUND, "<h1>gone</h1>");
            state.set_markdown(
                "/docs/bad-html.md",
                StatusCode::OK,
                &markdown_page("Bad Html", "Markdown survives."),
            );
            let site = add_site(state, None).await;
            sync(&site, false).await;
            site.server.abort();

            let rows = pages(&site);
            let status = |uri: &str| rows.iter().find(|p| p.0 == uri).map(|p| p.1.clone());
            assert_eq!(
                status("/docs/ok-html").as_deref(),
                Some("active"),
                "{rows:#?}"
            );
            assert_eq!(status("/docs/ok-html.md"), None, "{rows:#?}");
            assert_eq!(
                status("/docs/bad-html.md").as_deref(),
                Some("active"),
                "{rows:#?}"
            );
            let ok_md = &rows.iter().find(|p| p.0 == "/docs/ok-html").expect("row").3;
            assert!(ok_md.contains("Html body."), "{ok_md}");
        }

        fn page(url: &str, content_type: &str, body: &str) -> crate::ingest::web::Page {
            let mut page = hand_made_page(url);
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                content_type.parse().expect("content type"),
            );
            page.headers = Some(headers);
            page.set_html_bytes(Some(body.as_bytes().to_vec()));
            page
        }

        fn scope() -> crate::ingest::web::WebScope {
            let origin =
                crate::ingest::web::Url::parse("https://docs.example/docs/").expect("origin");
            crate::ingest::web::WebScope::new(&origin, &[], &[]).expect("scope")
        }

        #[test]
        fn reads_link_tags_from_minified_html() {
            let base = crate::ingest::web::Url::parse("https://docs.example/docs/guide?version=2")
                .expect("url");
            let links = crate::ingest::web::head_links(
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
            let pages = vec![
                page("https://docs.example/docs/", "text/html", html),
                page("https://docs.example/docs/a", "text/html", html),
                page("https://docs.example/docs/b", "text/html", html),
            ];
            let mut acc = crate::ingest::web::WebSyncAccounting::default();
            let (outcomes, ok_count, _) =
                crate::ingest::web::build_page_outcomes(pages, &scope(), &mut acc);
            assert_eq!(ok_count, 3);
            assert_eq!(outcomes.len(), 3);
            assert_eq!(acc.canonical_duplicate, 0);
        }

        #[test]
        fn twins_and_canonical_duplicates_are_counted_and_retired() {
            let html = r#"<html><head><link rel="canonical" href="/docs/guide"></head><body><h1>Guide</h1><p>Html text.</p></body></html>"#;
            let pages = vec![
                page(
                    "https://docs.example/docs/guide.md",
                    "text/markdown",
                    "# Guide\n\nMd text.\n",
                ),
                page("https://docs.example/docs/guide", "text/html", html),
                page(
                    "https://docs.example/docs/guide?version=2",
                    "text/html",
                    html,
                ),
                page(
                    "https://docs.example/docs/index.md",
                    "text/markdown",
                    "# Home\n\nMd home.\n",
                ),
                page(
                    "https://docs.example/docs/",
                    "text/html",
                    "<html><body><h1>Home</h1></body></html>",
                ),
            ];
            let mut acc = crate::ingest::web::WebSyncAccounting::default();
            let (outcomes, ok_count, _) =
                crate::ingest::web::build_page_outcomes(pages, &scope(), &mut acc);

            assert_eq!(ok_count, 2);
            assert_eq!(acc.markdown_twin, 2);
            assert_eq!(acc.canonical_duplicate, 1);
            assert_eq!(crate::ingest::web::result_count(&outcomes), 2);
            assert_eq!(
                5,
                acc.results_stored
                    + acc.results_failed
                    + acc.markdown_twin
                    + acc.canonical_duplicate,
                "every page is a result or one drop count: {acc:?}"
            );
            for outcome in &outcomes {
                if let crate::ingest::local::PageIngestOutcome::Ready { uri, doc, .. } = outcome {
                    assert!(!uri.ends_with(".md") && !uri.contains('?'), "{uri}");
                    assert!(doc.content_md.contains("Md "), "{uri}: {}", doc.content_md);
                }
            }
        }

        #[test]
        fn later_crawl_budget_counts_a_markdown_twin_pair_once() {
            let pages = vec![
                page("https://docs.example/docs/guide", "text/html", ""),
                page("https://docs.example/docs/guide.md", "text/markdown", ""),
                page("https://docs.example/docs/orphan.md", "text/markdown", ""),
                page("https://docs.example/docs/", "text/html", ""),
                page("https://docs.example/docs/index.md", "text/markdown", ""),
            ];
            assert_eq!(
                crate::ingest::web::count_scoped_documents(&pages, &scope()),
                3
            );
            assert_eq!(crate::ingest::web::count_scoped_urls(&pages, &scope()), 5);
        }

        #[test]
        fn a_failed_first_copy_of_a_page_claims_no_twin() {
            let mut failed = page("https://docs.example/docs/guide", "text/html", "");
            failed.status_code = StatusCode::NOT_FOUND;
            let pages = vec![
                failed,
                page(
                    "https://docs.example/docs/guide",
                    "text/html",
                    "<h1>Guide</h1>",
                ),
                page(
                    "https://docs.example/docs/guide.md",
                    "text/markdown",
                    "# Guide\n\nMd.\n",
                ),
            ];
            let mut acc = crate::ingest::web::WebSyncAccounting::default();
            let (outcomes, ok_count, _) =
                crate::ingest::web::build_page_outcomes(pages, &scope(), &mut acc);
            assert_eq!(acc.markdown_twin, 0, "{acc:?}");
            assert_eq!(ok_count, 1, "the .md is stored on its own");
            assert_eq!(crate::ingest::web::result_count(&outcomes), 2);
        }

        #[test]
        fn canonical_names_the_exact_trailing_slash_variant() {
            let html = r#"<html><head><link rel="canonical" href="/docs/p/"></head><body><h1>P</h1></body></html>"#;
            let pages = vec![
                page("https://docs.example/docs/p", "text/html", html),
                page("https://docs.example/docs/p/", "text/html", html),
            ];
            let mut acc = crate::ingest::web::WebSyncAccounting::default();
            let (outcomes, _, _) =
                crate::ingest::web::build_page_outcomes(pages, &scope(), &mut acc);
            let stored: Vec<&str> = outcomes
                .iter()
                .filter_map(|o| match o {
                    crate::ingest::local::PageIngestOutcome::Ready { uri, .. } => {
                        Some(uri.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(stored, vec!["https://docs.example/docs/p/"]);
            assert_eq!(acc.canonical_duplicate, 1);
        }
    }
}
