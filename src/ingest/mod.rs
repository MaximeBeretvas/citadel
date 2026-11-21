pub mod local;
pub mod normalize;
pub mod sections;
pub mod web;

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::VaultConfig;
use crate::db::open_rw_db;
use crate::error::{Result, VaultError};

pub use local::{canonicalize_local_dir, sync_local_source, SyncSummary};
pub use normalize::{
    count_words, extract_title, normalize_markdown, normalize_whitespace, sha256_hex,
    strip_volatile_frontmatter, unified_diff, NormalizedDocument, MAX_DIFF_BYTES,
};
pub use sections::{split_sections, SectionChunk};
pub use web::sync_web_source;

const DEFAULT_REFRESH_INTERVAL_SECS: i64 = 86_400;
const DEFAULT_MAX_PAGES: u32 = 500;
const MAX_ALLOWED_PAGES: u32 = 5_000;
const DEFAULT_MAX_DEPTH: u32 = 5;
const MAX_ALLOWED_DEPTH: u32 = 20;

/// Supported source kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Web,
    Local,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Local => "local",
        }
    }

    fn from_db(raw: &str) -> Result<Self> {
        match raw {
            "web" => Ok(Self::Web),
            "local" => Ok(Self::Local),
            other => Err(VaultError::Other(format!("unknown source kind '{other}'"))),
        }
    }
}

/// Stored row from `sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: i64,
    pub name: String,
    pub kind: SourceKind,
    pub origin: String,
    pub tags: Vec<String>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub max_pages: u32,
    pub max_depth: u32,
    pub render_js: bool,
    pub refresh_interval_secs: i64,
    pub status: String,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
}

/// Source row joined with live counts from `v_sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub id: i64,
    pub name: String,
    pub kind: SourceKind,
    pub origin: String,
    pub tags: Vec<String>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub max_pages: u32,
    pub max_depth: u32,
    pub render_js: bool,
    pub refresh_interval_secs: i64,
    pub status: String,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub active_pages: i64,
    pub stale_pages: i64,
    pub error_pages: i64,
    pub total_words: i64,
}

/// A single row from `sync_runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRunRecord {
    pub id: i64,
    pub source_id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub pages_added: i64,
    pub pages_updated: i64,
    pub pages_unchanged: i64,
    pub pages_stale: i64,
    pub pages_failed: i64,
    pub error_message: Option<String>,
}

/// Summary of a stale or failing page belonging to a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageIssueRecord {
    pub id: i64,
    pub uri: String,
    pub title: String,
    pub status: String,
    pub http_status: Option<i64>,
    pub error_message: Option<String>,
    pub last_checked_at: String,
}

/// Detailed source view including recent sync history and non-active pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDetail {
    pub source: SourceSummary,
    pub sync_runs: Vec<SyncRunRecord>,
    pub issue_pages: Vec<PageIssueRecord>,
}

/// Parameters for registering a new source.
#[derive(Debug, Clone, Default)]
pub struct AddSourceParams {
    pub origin: String,
    pub name: Option<String>,
    pub tags: Vec<String>,
    pub refresh_interval_secs: Option<i64>,
    pub max_pages: Option<u32>,
    pub max_depth: Option<u32>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub render_js: bool,
}

/// Partial update parameters for an existing source.
#[derive(Debug, Clone, Default)]
pub struct UpdateSourceParams {
    pub tags: Option<Vec<String>>,
    pub refresh_interval_secs: Option<i64>,
    pub max_pages: Option<u32>,
    pub max_depth: Option<u32>,
    pub include_patterns: Option<Vec<String>>,
    pub exclude_patterns: Option<Vec<String>>,
    pub render_js: Option<bool>,
}

/// Metadata returned when removing a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedSource {
    pub id: i64,
    pub name: String,
    pub pages_deleted: usize,
}

/// Parse a duration string such as `"0"`, `"30s"`, `"15m"`, `"24h"`, `"7d"`, `"2w"`, or raw seconds.
pub fn parse_duration_secs(input: &str) -> Result<i64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(VaultError::Validation("duration must not be empty".into()));
    }

    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return trimmed
            .parse::<i64>()
            .map_err(|_| VaultError::Validation(format!("duration '{input}' is out of range")));
    }

    let Some(unit) = trimmed.chars().last() else {
        return Err(VaultError::Validation("duration must not be empty".into()));
    };

    let multiplier: i64 = match unit.to_ascii_lowercase() {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 604_800,
        _ => {
            return Err(VaultError::Validation(format!(
                "invalid duration '{input}'; expected e.g. '0', '30s', '15m', '24h', '7d'"
            )));
        }
    };

    let num_part = &trimmed[..trimmed.len() - unit.len_utf8()];
    if num_part.is_empty() || !num_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(VaultError::Validation(format!(
            "invalid duration '{input}'; expected non-negative integer before unit '{unit}'"
        )));
    }

    let qty = num_part
        .parse::<i64>()
        .map_err(|_| VaultError::Validation(format!("duration '{input}' is out of range")))?;

    qty.checked_mul(multiplier)
        .ok_or_else(|| VaultError::Validation(format!("duration '{input}' overflows i64")))
}

/// Register a new source and optionally perform an initial sync.
pub fn add_source(
    config: &VaultConfig,
    params: AddSourceParams,
    sync_now: bool,
) -> Result<(SourceRecord, Option<SyncSummary>)> {
    let record = register_source_row(config, params)?;
    if !sync_now {
        return Ok((record, None));
    }

    let summary = match record.kind {
        SourceKind::Local => {
            let mut conn = open_rw_db(&config.db_path())?;
            sync_local_source(
                &mut conn,
                record.id,
                &record.name,
                Path::new(&record.origin),
                &record.include_patterns,
                &record.exclude_patterns,
                false,
            )?
        }
        SourceKind::Web => run_async_blocking(sync_web_source(&config.db_path(), &record, false))?,
    };
    let conn = open_rw_db(&config.db_path())?;
    let updated = load_source_by_id(&conn, record.id)?;
    Ok((updated, Some(summary)))
}

/// Async variant of [`add_source`] supporting immediate web crawling without blocking an async executor.
pub async fn add_source_async(
    config: &VaultConfig,
    params: AddSourceParams,
    sync_now: bool,
) -> Result<(SourceRecord, Option<SyncSummary>)> {
    let record = register_source_row(config, params)?;
    if !sync_now {
        return Ok((record, None));
    }

    let summary = match record.kind {
        SourceKind::Local => {
            let mut conn = open_rw_db(&config.db_path())?;
            sync_local_source(
                &mut conn,
                record.id,
                &record.name,
                Path::new(&record.origin),
                &record.include_patterns,
                &record.exclude_patterns,
                false,
            )?
        }
        SourceKind::Web => sync_web_source(&config.db_path(), &record, false).await?,
    };

    let conn = open_rw_db(&config.db_path())?;
    let updated = load_source_by_id(&conn, record.id)?;
    Ok((updated, Some(summary)))
}

fn register_source_row(config: &VaultConfig, params: AddSourceParams) -> Result<SourceRecord> {
    let (kind, normalized_origin) = classify_origin(&params.origin)?;
    let refresh_interval_secs = params
        .refresh_interval_secs
        .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS);
    let max_pages = params.max_pages.unwrap_or(DEFAULT_MAX_PAGES);
    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);

    validate_source_limits(refresh_interval_secs, max_pages, max_depth)?;
    local::validate_patterns(&params.include_patterns)?;
    local::validate_patterns(&params.exclude_patterns)?;

    let tags = normalize_string_list(params.tags);
    let includes = normalize_string_list(params.include_patterns);
    let excludes = normalize_string_list(params.exclude_patterns);

    let conn = open_rw_db(&config.db_path())?;

    if let Some(existing_name) = conn
        .query_row(
            "SELECT name FROM sources WHERE origin = ?1",
            [&normalized_origin],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Err(VaultError::Validation(format!(
            "origin '{normalized_origin}' is already registered as source '{existing_name}'"
        )));
    }

    let slug = resolve_unique_slug(&conn, kind, &normalized_origin, params.name.as_deref())?;
    let tags_json = serialize_json_list(&tags)?;
    let includes_json = serialize_json_list(&includes)?;
    let excludes_json = serialize_json_list(&excludes)?;

    conn.execute(
        "INSERT INTO sources (
             name, kind, origin, tags, include_patterns, exclude_patterns,
             max_pages, max_depth, render_js, refresh_interval_secs
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            slug,
            kind.as_str(),
            normalized_origin,
            tags_json,
            includes_json,
            excludes_json,
            i64::from(max_pages),
            i64::from(max_depth),
            i64::from(params.render_js),
            refresh_interval_secs,
        ],
    )?;

    let source_id = conn.last_insert_rowid();
    load_source_by_id(&conn, source_id)
}

/// List all sources with live page and word counts from `v_sources`.
pub fn list_sources(config: &VaultConfig) -> Result<Vec<SourceSummary>> {
    if !config.db_path().exists() {
        return Ok(Vec::new());
    }
    let conn = open_rw_db(&config.db_path())?;
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at,
                active_pages, stale_pages, error_pages, total_words
         FROM v_sources
         ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], map_source_summary)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Retrieve source details, recent sync runs, and stale/error pages for `name`.
pub fn show_source(config: &VaultConfig, name: &str) -> Result<SourceDetail> {
    if !config.db_path().exists() {
        return Err(VaultError::NotFound(format!("source '{name}' not found")));
    }
    let conn = open_rw_db(&config.db_path())?;
    let source = conn
        .query_row(
            "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                    max_pages, max_depth, render_js, refresh_interval_secs,
                    status, last_synced_at, last_error, created_at,
                    active_pages, stale_pages, error_pages, total_words
             FROM v_sources
             WHERE name = ?1",
            [name],
            map_source_summary,
        )
        .optional()?
        .ok_or_else(|| VaultError::NotFound(format!("source '{name}' not found")))?;

    let mut runs_stmt = conn.prepare(
        "SELECT id, source_id, started_at, finished_at, status,
                pages_added, pages_updated, pages_unchanged, pages_stale, pages_failed, error_message
         FROM sync_runs
         WHERE source_id = ?1
         ORDER BY id DESC
         LIMIT 10",
    )?;
    let sync_runs = runs_stmt
        .query_map([source.id], |row| {
            Ok(SyncRunRecord {
                id: row.get(0)?,
                source_id: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                status: row.get(4)?,
                pages_added: row.get(5)?,
                pages_updated: row.get(6)?,
                pages_unchanged: row.get(7)?,
                pages_stale: row.get(8)?,
                pages_failed: row.get(9)?,
                error_message: row.get(10)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut issues_stmt = conn.prepare(
        "SELECT id, uri, title, status, http_status, error_message, last_checked_at
         FROM pages
         WHERE source_id = ?1 AND status IN ('stale', 'error')
         ORDER BY uri ASC",
    )?;
    let issue_pages = issues_stmt
        .query_map([source.id], |row| {
            Ok(PageIssueRecord {
                id: row.get(0)?,
                uri: row.get(1)?,
                title: row.get(2)?,
                status: row.get(3)?,
                http_status: row.get(4)?,
                error_message: row.get(5)?,
                last_checked_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(SourceDetail {
        source,
        sync_runs,
        issue_pages,
    })
}

/// Sync a named source or all registered sources.
pub fn sync_sources(
    config: &VaultConfig,
    name: Option<&str>,
    all: bool,
    prune: bool,
) -> Result<Vec<SyncSummary>> {
    let targets = load_sync_targets(config, name, all)?;
    let mut summaries = Vec::with_capacity(targets.len());

    for src in targets {
        match src.kind {
            SourceKind::Local => {
                let mut conn = open_rw_db(&config.db_path())?;
                match sync_local_source(
                    &mut conn,
                    src.id,
                    &src.name,
                    Path::new(&src.origin),
                    &src.include_patterns,
                    &src.exclude_patterns,
                    prune,
                ) {
                    Ok(summary) => summaries.push(summary),
                    Err(err) => {
                        record_pre_walk_error(&conn, src.id, &err.to_string());
                        return Err(err);
                    }
                }
            }
            SourceKind::Web => {
                let summary = run_async_blocking(sync_web_source(&config.db_path(), &src, prune))?;
                summaries.push(summary);
            }
        }
    }

    Ok(summaries)
}

/// Async variant of [`sync_sources`] supporting web crawling inside an existing Tokio runtime.
pub async fn sync_sources_async(
    config: &VaultConfig,
    name: Option<&str>,
    all: bool,
    prune: bool,
) -> Result<Vec<SyncSummary>> {
    let targets = load_sync_targets(config, name, all)?;
    let mut summaries = Vec::with_capacity(targets.len());

    for src in targets {
        match src.kind {
            SourceKind::Local => {
                let mut conn = open_rw_db(&config.db_path())?;
                match sync_local_source(
                    &mut conn,
                    src.id,
                    &src.name,
                    Path::new(&src.origin),
                    &src.include_patterns,
                    &src.exclude_patterns,
                    prune,
                ) {
                    Ok(summary) => summaries.push(summary),
                    Err(err) => {
                        record_pre_walk_error(&conn, src.id, &err.to_string());
                        return Err(err);
                    }
                }
            }
            SourceKind::Web => {
                let summary = sync_web_source(&config.db_path(), &src, prune).await?;
                summaries.push(summary);
            }
        }
    }

    Ok(summaries)
}

fn load_sync_targets(
    config: &VaultConfig,
    name: Option<&str>,
    all: bool,
) -> Result<Vec<SourceRecord>> {
    if name.is_some() && all {
        return Err(VaultError::Validation(
            "cannot specify both a source <name> and --all".into(),
        ));
    }
    if name.is_none() && !all {
        return Err(VaultError::Validation(
            "specify a source <name> or pass --all".into(),
        ));
    }

    if !config.db_path().exists() {
        if let Some(target_name) = name {
            return Err(VaultError::NotFound(format!(
                "source '{target_name}' not found"
            )));
        }
        return Ok(Vec::new());
    }

    let conn = open_rw_db(&config.db_path())?;
    if let Some(target_name) = name {
        Ok(vec![load_source_by_name(&conn, target_name)?])
    } else {
        load_all_sources(&conn)
    }
}

fn run_async_blocking<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send,
    T: Send,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .map_err(|err| {
                            VaultError::Other(format!("failed to build tokio runtime: {err}"))
                        })?;
                    rt.block_on(fut)
                })
                .join()
                .map_err(|_| VaultError::Other("async ingestion thread panicked".into()))?
        })
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|err| VaultError::Other(format!("failed to build tokio runtime: {err}")))?;
        rt.block_on(fut)
    }
}

/// Update mutable configuration fields for an existing source.
pub fn update_source(
    config: &VaultConfig,
    name: &str,
    params: UpdateSourceParams,
) -> Result<SourceRecord> {
    if !config.db_path().exists() {
        return Err(VaultError::NotFound(format!("source '{name}' not found")));
    }

    let conn = open_rw_db(&config.db_path())?;
    let existing = load_source_by_name(&conn, name)?;

    let tags = params
        .tags
        .map(normalize_string_list)
        .unwrap_or(existing.tags);
    let includes = match params.include_patterns {
        Some(list) => {
            local::validate_patterns(&list)?;
            normalize_string_list(list)
        }
        None => existing.include_patterns,
    };
    let excludes = match params.exclude_patterns {
        Some(list) => {
            local::validate_patterns(&list)?;
            normalize_string_list(list)
        }
        None => existing.exclude_patterns,
    };

    let refresh_interval_secs = params
        .refresh_interval_secs
        .unwrap_or(existing.refresh_interval_secs);
    let max_pages = params.max_pages.unwrap_or(existing.max_pages);
    let max_depth = params.max_depth.unwrap_or(existing.max_depth);
    let render_js = params.render_js.unwrap_or(existing.render_js);

    validate_source_limits(refresh_interval_secs, max_pages, max_depth)?;

    let tags_json = serialize_json_list(&tags)?;
    let includes_json = serialize_json_list(&includes)?;
    let excludes_json = serialize_json_list(&excludes)?;

    conn.execute(
        "UPDATE sources
         SET tags = ?2,
             include_patterns = ?3,
             exclude_patterns = ?4,
             max_pages = ?5,
             max_depth = ?6,
             render_js = ?7,
             refresh_interval_secs = ?8
         WHERE id = ?1",
        params![
            existing.id,
            tags_json,
            includes_json,
            excludes_json,
            i64::from(max_pages),
            i64::from(max_depth),
            i64::from(render_js),
            refresh_interval_secs,
        ],
    )?;

    load_source_by_id(&conn, existing.id)
}

/// Remove a source by slug name, cascading stored pages and returning deleted count.
pub fn remove_source(config: &VaultConfig, name: &str) -> Result<RemovedSource> {
    if !config.db_path().exists() {
        return Err(VaultError::NotFound(format!("source '{name}' not found")));
    }

    let mut conn = open_rw_db(&config.db_path())?;
    let tx = conn.transaction()?;

    let source_id: i64 = tx
        .query_row("SELECT id FROM sources WHERE name = ?1", [name], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| VaultError::NotFound(format!("source '{name}' not found")))?;

    let pages_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pages WHERE source_id = ?1",
        [source_id],
        |r| r.get(0),
    )?;

    tx.execute("DELETE FROM sources WHERE id = ?1", [source_id])?;
    tx.commit()?;

    Ok(RemovedSource {
        id: source_id,
        name: name.to_string(),
        pages_deleted: usize::try_from(pages_count).unwrap_or(0),
    })
}

fn record_pre_walk_error(conn: &Connection, source_id: i64, message: &str) {
    let has_running: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE source_id = ?1 AND status = 'running'",
            [source_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .unwrap_or(false);

    if !has_running {
        let _ = conn.execute(
            "INSERT INTO sync_runs (source_id, started_at, finished_at, status, error_message)
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'error', ?2)",
            params![source_id, message],
        );
    }
    let _ = conn.execute(
        "UPDATE sources SET status = 'error', last_error = ?2 WHERE id = ?1",
        params![source_id, message],
    );
}

fn classify_origin(raw_origin: &str) -> Result<(SourceKind, String)> {
    let trimmed = raw_origin.trim();
    if trimmed.is_empty() {
        return Err(VaultError::Validation(
            "origin must not be empty; pass an http(s):// URL or local directory path".into(),
        ));
    }

    if trimmed.contains("://") {
        let parsed = Url::parse(trimmed).map_err(|err| {
            VaultError::Validation(format!("invalid origin URL '{trimmed}': {err}"))
        })?;

        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(VaultError::Validation(format!(
                "origin URL '{trimmed}' must use http:// or https:// with a valid host"
            )));
        }

        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(VaultError::Validation(
                "origin URL must not contain embedded user credentials".into(),
            ));
        }

        return Ok((SourceKind::Web, parsed.into()));
    }

    let canon = canonicalize_local_dir(Path::new(trimmed))?;
    let canon_str = canon.to_str().ok_or_else(|| {
        VaultError::Validation(format!(
            "local origin directory '{}' contains non-UTF-8 characters",
            canon.display()
        ))
    })?;
    Ok((SourceKind::Local, canon_str.to_string()))
}

fn validate_source_limits(
    refresh_interval_secs: i64,
    max_pages: u32,
    max_depth: u32,
) -> Result<()> {
    if refresh_interval_secs < 0 {
        return Err(VaultError::Validation(
            "refresh_interval_secs must be >= 0".into(),
        ));
    }
    if !(1..=MAX_ALLOWED_PAGES).contains(&max_pages) {
        return Err(VaultError::Validation(format!(
            "max_pages must be between 1 and {MAX_ALLOWED_PAGES}"
        )));
    }
    if !(1..=MAX_ALLOWED_DEPTH).contains(&max_depth) {
        return Err(VaultError::Validation(format!(
            "max_depth must be between 1 and {MAX_ALLOWED_DEPTH}"
        )));
    }
    Ok(())
}

fn validate_explicit_slug(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VaultError::Validation(
            "source name must not be empty".into(),
        ));
    }
    if trimmed.starts_with('.') {
        return Err(VaultError::Validation(
            "source name must not start with '.'".into(),
        ));
    }
    let valid_chars = trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    let has_alnum = trimmed.chars().any(|ch| ch.is_ascii_alphanumeric());
    if !valid_chars || !has_alnum {
        return Err(VaultError::Validation(format!(
            "invalid source name '{trimmed}'; use ASCII letters, digits, '-', '_', or '.'"
        )));
    }
    Ok(trimmed.to_string())
}

fn slugify_text(raw: &str) -> String {
    let mut slug = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn derive_slug(kind: SourceKind, origin: &str) -> String {
    let derived = match kind {
        SourceKind::Local => {
            let folder_name = Path::new(origin)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            slugify_text(&folder_name)
        }
        SourceKind::Web => match Url::parse(origin) {
            Ok(url) => {
                let mut parts: Vec<String> = Vec::new();
                if let Some(host) = url.host_str() {
                    parts.push(host.to_string());
                }
                if let Some(segments) = url.path_segments() {
                    for seg in segments.filter(|s| !s.is_empty()).take(2) {
                        parts.push(seg.to_string());
                    }
                }
                slugify_text(&parts.join("-"))
            }
            Err(_) => String::new(),
        },
    };

    if derived.is_empty() {
        match kind {
            SourceKind::Local => "local-source".to_string(),
            SourceKind::Web => "web-source".to_string(),
        }
    } else {
        derived
    }
}

fn resolve_unique_slug(
    conn: &Connection,
    kind: SourceKind,
    origin: &str,
    explicit_name: Option<&str>,
) -> Result<String> {
    if let Some(raw_name) = explicit_name {
        let slug = validate_explicit_slug(raw_name)?;
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sources WHERE name = ?1",
            [&slug],
            |r| r.get(0),
        )?;
        if exists {
            return Err(VaultError::Validation(format!(
                "source name '{slug}' already exists"
            )));
        }
        return Ok(slug);
    }

    let base = derive_slug(kind, origin);
    let mut candidate = base.clone();
    let mut suffix = 2usize;

    loop {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sources WHERE name = ?1",
            [&candidate],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
}

fn normalize_string_list(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn serialize_json_list(items: &[String]) -> Result<String> {
    serde_json::to_string(items)
        .map_err(|err| VaultError::Other(format!("failed to serialize list: {err}")))
}

fn parse_json_list(raw: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw)
        .map_err(|err| VaultError::Other(format!("failed to decode stored JSON list: {err}")))
}

pub(crate) fn map_source_record(row: &Row<'_>) -> rusqlite::Result<SourceRecord> {
    let kind_raw: String = row.get(2)?;
    let tags_raw: String = row.get(4)?;
    let includes_raw: String = row.get(5)?;
    let excludes_raw: String = row.get(6)?;
    let max_pages: i64 = row.get(7)?;
    let max_depth: i64 = row.get(8)?;
    let render_js: i64 = row.get(9)?;

    let kind = SourceKind::from_db(&kind_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let tags = parse_json_list(&tags_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let include_patterns = parse_json_list(&includes_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let exclude_patterns = parse_json_list(&excludes_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(SourceRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        kind,
        origin: row.get(3)?,
        tags,
        include_patterns,
        exclude_patterns,
        max_pages: u32::try_from(max_pages).unwrap_or(DEFAULT_MAX_PAGES),
        max_depth: u32::try_from(max_depth).unwrap_or(DEFAULT_MAX_DEPTH),
        render_js: render_js != 0,
        refresh_interval_secs: row.get(10)?,
        status: row.get(11)?,
        last_synced_at: row.get(12)?,
        last_error: row.get(13)?,
        created_at: row.get(14)?,
    })
}

fn map_source_summary(row: &Row<'_>) -> rusqlite::Result<SourceSummary> {
    let base = map_source_record(row)?;
    Ok(SourceSummary {
        id: base.id,
        name: base.name,
        kind: base.kind,
        origin: base.origin,
        tags: base.tags,
        include_patterns: base.include_patterns,
        exclude_patterns: base.exclude_patterns,
        max_pages: base.max_pages,
        max_depth: base.max_depth,
        render_js: base.render_js,
        refresh_interval_secs: base.refresh_interval_secs,
        status: base.status,
        last_synced_at: base.last_synced_at,
        last_error: base.last_error,
        created_at: base.created_at,
        active_pages: row.get(15)?,
        stale_pages: row.get(16)?,
        error_pages: row.get(17)?,
        total_words: row.get(18)?,
    })
}

fn load_source_by_id(conn: &Connection, id: i64) -> Result<SourceRecord> {
    conn.query_row(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at
         FROM sources WHERE id = ?1",
        [id],
        map_source_record,
    )
    .optional()?
    .ok_or_else(|| VaultError::NotFound(format!("source id {id} not found")))
}

fn load_source_by_name(conn: &Connection, name: &str) -> Result<SourceRecord> {
    conn.query_row(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at
         FROM sources WHERE name = ?1",
        [name],
        map_source_record,
    )
    .optional()?
    .ok_or_else(|| VaultError::NotFound(format!("source '{name}' not found")))
}

fn load_all_sources(conn: &Connection) -> Result<Vec<SourceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at
         FROM sources ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], map_source_record)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_durations_cleanly() {
        assert_eq!(parse_duration_secs("0").expect("0"), 0);
        assert_eq!(parse_duration_secs("45").expect("45"), 45);
        assert_eq!(parse_duration_secs("30s").expect("30s"), 30);
        assert_eq!(parse_duration_secs("15m").expect("15m"), 900);
        assert_eq!(parse_duration_secs("24H").expect("24H"), 86_400);
        assert_eq!(parse_duration_secs("7d").expect("7d"), 604_800);
        assert_eq!(parse_duration_secs("1w").expect("1w"), 604_800);

        assert!(matches!(
            parse_duration_secs("").unwrap_err(),
            VaultError::Validation(_)
        ));
        assert!(matches!(
            parse_duration_secs("-5m").unwrap_err(),
            VaultError::Validation(_)
        ));
        assert!(matches!(
            parse_duration_secs("10x").unwrap_err(),
            VaultError::Validation(_)
        ));
    }

    #[test]
    fn add_auto_detects_local_and_web_with_no_sync() {
        let vault_dir = tempdir().expect("vault tempdir");
        let docs_dir = tempdir().expect("docs tempdir");
        std::fs::write(docs_dir.path().join("intro.md"), "# Intro\n\nHello.\n").expect("write");

        let cfg = VaultConfig::new(vault_dir.path());

        let (web_rec, web_sum) = add_source(
            &cfg,
            AddSourceParams {
                origin: "https://example.com/docs".to_string(),
                ..Default::default()
            },
            false,
        )
        .expect("--no-sync should register web source");
        assert_eq!(web_rec.kind, SourceKind::Web);
        assert_eq!(web_rec.name, "example-com-docs");
        assert!(web_sum.is_none());

        let (local_rec, local_sum) = add_source(
            &cfg,
            AddSourceParams {
                origin: docs_dir.path().to_string_lossy().into_owned(),
                tags: vec!["books".to_string()],
                ..Default::default()
            },
            true,
        )
        .expect("local add should sync immediately");
        assert_eq!(local_rec.kind, SourceKind::Local);
        assert_eq!(local_rec.status, "ok");
        assert_eq!(local_sum.expect("summary").pages_added, 1);

        let updated = update_source(
            &cfg,
            &local_rec.name,
            UpdateSourceParams {
                tags: Some(vec!["books".to_string(), "rust".to_string()]),
                refresh_interval_secs: Some(0),
                ..Default::default()
            },
        )
        .expect("update_source");
        assert_eq!(updated.tags, vec!["books", "rust"]);
        assert_eq!(updated.refresh_interval_secs, 0);
    }
}
