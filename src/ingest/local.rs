use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use glob::Pattern;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{Result, VaultError};
use crate::ingest::normalize::{normalize_markdown, unified_diff};
use crate::ingest::sections::split_sections;

/// Summary metrics produced by syncing a single source.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncSummary {
    pub source_name: String,
    pub pages_added: usize,
    pub pages_updated: usize,
    pub pages_unchanged: usize,
    pub pages_stale: usize,
    pub pages_failed: usize,
    pub pruned: usize,
}

#[derive(Debug, Clone)]
enum CompiledPattern {
    Prefix(String),
    Glob(Pattern),
}

impl CompiledPattern {
    fn compile(raw: &str) -> Result<Self> {
        let trimmed = raw.trim().trim_start_matches("./").trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(VaultError::Validation(
                "include/exclude pattern must not be empty".into(),
            ));
        }
        if trimmed.contains(['*', '?', '[']) {
            let pattern = Pattern::new(trimmed).map_err(|err| {
                VaultError::Validation(format!("invalid glob pattern '{raw}': {err}"))
            })?;
            Ok(Self::Glob(pattern))
        } else {
            Ok(Self::Prefix(trimmed.to_string()))
        }
    }

    fn matches(&self, uri: &str) -> bool {
        match self {
            Self::Glob(pat) => pat.matches(uri),
            Self::Prefix(prefix) => {
                let clean = prefix.trim_end_matches('/');
                uri == prefix || uri == clean || uri.starts_with(&format!("{clean}/"))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PatternMatcher {
    includes: Vec<CompiledPattern>,
    excludes: Vec<CompiledPattern>,
}

impl PatternMatcher {
    pub(crate) fn new(includes: &[String], excludes: &[String]) -> Result<Self> {
        let includes = includes
            .iter()
            .map(|p| CompiledPattern::compile(p))
            .collect::<Result<Vec<_>>>()?;
        let excludes = excludes
            .iter()
            .map(|p| CompiledPattern::compile(p))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { includes, excludes })
    }

    pub(crate) fn matches(&self, uri: &str) -> bool {
        let included = self.includes.is_empty() || self.includes.iter().any(|pat| pat.matches(uri));
        if !included {
            return false;
        }
        !self.excludes.iter().any(|pat| pat.matches(uri))
    }
}

/// Validate that all glob/prefix patterns compile cleanly.
pub(crate) fn validate_patterns(patterns: &[String]) -> Result<()> {
    for pat in patterns {
        CompiledPattern::compile(pat)?;
    }
    Ok(())
}

/// Validate that `origin` exists and is a directory, returning its canonical path.
pub fn canonicalize_local_dir(origin: &Path) -> Result<PathBuf> {
    if origin.as_os_str().is_empty() {
        return Err(VaultError::Validation(
            "local origin directory path must not be empty".into(),
        ));
    }

    let canonical = fs::canonicalize(origin).map_err(|err| {
        VaultError::Validation(format!(
            "local origin directory '{}' is invalid: {err}",
            origin.display()
        ))
    })?;

    if !canonical.is_dir() {
        return Err(VaultError::Validation(format!(
            "local origin '{}' is not a directory",
            origin.display()
        )));
    }

    Ok(canonical)
}

/// Synchronize a local Markdown directory into `pages`, `sections`, `page_revisions`, and `sync_runs`.
pub fn sync_local_source(
    conn: &mut Connection,
    source_id: i64,
    source_name: &str,
    origin_dir: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    prune: bool,
) -> Result<SyncSummary> {
    let canon_root = canonicalize_local_dir(origin_dir)?;
    let matcher = PatternMatcher::new(include_patterns, exclude_patterns)?;

    conn.execute(
        "UPDATE sources SET status = 'syncing', last_error = NULL WHERE id = ?1",
        [source_id],
    )?;
    conn.execute(
        "INSERT INTO sync_runs (source_id, started_at, status)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'running')",
        [source_id],
    )?;
    let run_id = conn.last_insert_rowid();

    match execute_local_sync(
        conn,
        source_id,
        source_name,
        run_id,
        &canon_root,
        &matcher,
        prune,
    ) {
        Ok(summary) => Ok(summary),
        Err(err) => {
            let msg = err.to_string();
            let _ = conn.execute(
                "UPDATE sync_runs
                 SET finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                     status = 'error',
                     error_message = ?2
                 WHERE id = ?1",
                params![run_id, msg],
            );
            let _ = conn.execute(
                "UPDATE sources SET status = 'error', last_error = ?2 WHERE id = ?1",
                params![source_id, msg],
            );
            Err(err)
        }
    }
}

fn execute_local_sync(
    conn: &mut Connection,
    source_id: i64,
    source_name: &str,
    run_id: i64,
    canon_root: &Path,
    matcher: &PatternMatcher,
    prune: bool,
) -> Result<SyncSummary> {
    let files = collect_markdown_files(canon_root, matcher)?;

    let mut summary = SyncSummary {
        source_name: source_name.to_string(),
        ..Default::default()
    };
    let mut seen_uris: HashSet<String> = HashSet::with_capacity(files.len());

    let tx = conn.transaction()?;

    for (uri, abs_path) in files {
        seen_uris.insert(uri.clone());

        let raw_bytes = match fs::read(&abs_path) {
            Ok(bytes) => bytes,
            Err(io_err) => {
                tracing::warn!(
                    source = source_name,
                    uri = %uri,
                    error = %io_err,
                    "failed to read markdown file"
                );
                summary.pages_failed += 1;
                tx.execute(
                    "UPDATE pages
                     SET status = 'error',
                         error_message = ?1,
                         last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                     WHERE source_id = ?2 AND uri = ?3",
                    params![io_err.to_string(), source_id, uri],
                )?;
                continue;
            }
        };

        let raw_md = match String::from_utf8(raw_bytes) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(
                    source = source_name,
                    uri = %uri,
                    "markdown file is not strict UTF-8; decoding lossily"
                );
                String::from_utf8_lossy(err.as_bytes()).into_owned()
            }
        };

        let fallback_title = abs_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uri.clone());

        let doc = normalize_markdown(&raw_md, &fallback_title);
        let chunks = split_sections(&doc.content_md);

        let existing: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT id, content_hash, content_md, status
                 FROM pages
                 WHERE source_id = ?1 AND uri = ?2",
                params![source_id, uri],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        match existing {
            None => {
                tx.execute(
                    "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')",
                    params![
                        source_id,
                        uri,
                        doc.title,
                        doc.content_md,
                        doc.content_hash,
                        i64::try_from(doc.word_count).unwrap_or(i64::MAX),
                    ],
                )?;
                let page_id = tx.last_insert_rowid();
                insert_sections(&tx, page_id, source_id, &chunks)?;
                summary.pages_added += 1;
            }
            Some((page_id, old_hash, old_content_md, old_status)) => {
                if old_hash == doc.content_hash {
                    if old_status != "active" {
                        tx.execute(
                            "UPDATE pages
                             SET status = 'active',
                                 error_message = NULL,
                                 last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                             WHERE id = ?1",
                            [page_id],
                        )?;
                    } else {
                        tx.execute(
                            "UPDATE pages
                             SET last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                             WHERE id = ?1",
                            [page_id],
                        )?;
                    }
                    summary.pages_unchanged += 1;
                } else {
                    let diff = unified_diff(&old_content_md, &doc.content_md);
                    tx.execute(
                        "INSERT INTO page_revisions (page_id, previous_hash, new_hash, unified_diff, changed_at)
                         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         ON CONFLICT(page_id) DO UPDATE SET
                             previous_hash = excluded.previous_hash,
                             new_hash = excluded.new_hash,
                             unified_diff = excluded.unified_diff,
                             changed_at = excluded.changed_at",
                        params![page_id, old_hash, doc.content_hash, diff],
                    )?;
                    tx.execute(
                        "UPDATE pages
                         SET title = ?2,
                             content_md = ?3,
                             content_hash = ?4,
                             word_count = ?5,
                             status = 'active',
                             error_message = NULL,
                             last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                             last_changed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                         WHERE id = ?1",
                        params![
                            page_id,
                            doc.title,
                            doc.content_md,
                            doc.content_hash,
                            i64::try_from(doc.word_count).unwrap_or(i64::MAX),
                        ],
                    )?;
                    tx.execute("DELETE FROM sections WHERE page_id = ?1", [page_id])?;
                    insert_sections(&tx, page_id, source_id, &chunks)?;
                    summary.pages_updated += 1;
                }
            }
        }
    }

    reconcile_missing_pages(&tx, source_id, &seen_uris, prune, &mut summary)?;

    tx.execute(
        "UPDATE sync_runs
         SET finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             status = 'ok',
             pages_added = ?2,
             pages_updated = ?3,
             pages_unchanged = ?4,
             pages_stale = ?5,
             pages_failed = ?6
         WHERE id = ?1",
        params![
            run_id,
            i64::try_from(summary.pages_added).unwrap_or(i64::MAX),
            i64::try_from(summary.pages_updated).unwrap_or(i64::MAX),
            i64::try_from(summary.pages_unchanged).unwrap_or(i64::MAX),
            i64::try_from(summary.pages_stale).unwrap_or(i64::MAX),
            i64::try_from(summary.pages_failed).unwrap_or(i64::MAX),
        ],
    )?;

    tx.execute(
        "UPDATE sources
         SET status = 'ok',
             last_synced_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             last_error = NULL
         WHERE id = ?1",
        [source_id],
    )?;

    tx.commit()?;
    Ok(summary)
}

fn insert_sections(
    tx: &rusqlite::Transaction<'_>,
    page_id: i64,
    source_id: i64,
    chunks: &[crate::ingest::sections::SectionChunk],
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for chunk in chunks {
        stmt.execute(params![
            page_id,
            source_id,
            i64::try_from(chunk.ordinal).unwrap_or(i64::MAX),
            chunk.heading,
            chunk.heading_path,
            i64::from(chunk.level),
            chunk.content_md,
            i64::try_from(chunk.word_count).unwrap_or(i64::MAX),
        ])?;
    }
    Ok(())
}

fn reconcile_missing_pages(
    tx: &rusqlite::Transaction<'_>,
    source_id: i64,
    seen_uris: &HashSet<String>,
    prune: bool,
    summary: &mut SyncSummary,
) -> Result<()> {
    let missing: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, uri, status
             FROM pages
             WHERE source_id = ?1",
        )?;
        let rows = stmt.query_map([source_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut collected = Vec::new();
        for row in rows {
            let (id, uri, status) = row?;
            if !seen_uris.contains(&uri) {
                collected.push((id, status));
            }
        }
        collected
    };

    for (page_id, status) in missing {
        if prune {
            tx.execute("DELETE FROM pages WHERE id = ?1", [page_id])?;
            summary.pruned += 1;
        } else if status == "active" || status == "error" {
            tx.execute(
                "UPDATE pages
                 SET status = 'stale',
                     last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                 WHERE id = ?1",
                [page_id],
            )?;
            summary.pages_stale += 1;
        }
    }

    Ok(())
}

fn collect_markdown_files(root: &Path, matcher: &PatternMatcher) -> Result<Vec<(String, PathBuf)>> {
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    visited_dirs.insert(root.to_path_buf());

    let mut files = Vec::new();
    walk_dir(root, root, &mut visited_dirs, matcher, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    visited_dirs: &mut HashSet<PathBuf>,
    matcher: &PatternMatcher,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let name_lossy = name.to_string_lossy();
        if should_skip_entry(&name_lossy) {
            continue;
        }

        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            let canon_sub = fs::canonicalize(&path)?;
            if visited_dirs.insert(canon_sub) {
                walk_dir(root, &path, visited_dirs, matcher, out)?;
            }
            continue;
        }

        if file_type.is_symlink() {
            match fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => {
                    let canon_sub = fs::canonicalize(&path)?;
                    if visited_dirs.insert(canon_sub) {
                        walk_dir(root, &path, visited_dirs, matcher, out)?;
                    }
                    continue;
                }
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(_) => {
                    // Preserve broken symlinks with Markdown extensions so sync tracks them as `pages_failed`.
                }
            }
        } else if !file_type.is_file() {
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        let rel = path.strip_prefix(root).map_err(|err| {
            VaultError::Other(format!(
                "failed to compute relative path for '{}': {err}",
                path.display()
            ))
        })?;
        let uri = relative_path_to_uri(rel)?;
        if matcher.matches(&uri) {
            out.push((uri, path));
        }
    }

    Ok(())
}

fn should_skip_entry(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn relative_path_to_uri(rel: &Path) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        if let Component::Normal(os_str) = component {
            let part = os_str.to_str().ok_or_else(|| {
                VaultError::Validation(format!("non-UTF-8 path segment in '{}'", rel.display()))
            })?;
            parts.push(part.to_string());
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_rw_db;
    use tempfile::tempdir;

    fn insert_test_source(conn: &Connection, name: &str, origin: &Path) -> i64 {
        conn.execute(
            "INSERT INTO sources (name, kind, origin) VALUES (?1, 'local', ?2)",
            rusqlite::params![name, origin.to_string_lossy().as_ref()],
        )
        .expect("insert source should succeed");
        conn.last_insert_rowid()
    }

    #[test]
    fn ingests_nested_markdown_and_ignores_git_hidden_dirs() {
        let vault_dir = tempdir().expect("tempdir should succeed");
        let docs_dir = tempdir().expect("docs tempdir should succeed");
        let root = docs_dir.path();

        std::fs::create_dir_all(root.join("chapters/nested")).expect("create nested dir");
        std::fs::create_dir_all(root.join(".git/objects")).expect("create .git dir");
        std::fs::create_dir_all(root.join("node_modules/pkg")).expect("create node_modules dir");
        std::fs::create_dir_all(root.join(".hidden-dir")).expect("create hidden dir");

        std::fs::write(
            root.join("README.md"),
            "# Vault Book\n\nRoot overview document.\n",
        )
        .expect("write README.md");
        std::fs::write(
            root.join("chapters/ch01.markdown"),
            "---\ntitle: Chapter One\n---\n\n## Basics\n\nContent for chapter one.\n",
        )
        .expect("write ch01.markdown");
        std::fs::write(
            root.join("chapters/nested/deep.MD"),
            "# Deep Topic\n\nNested uppercase extension.\n",
        )
        .expect("write deep.MD");

        // Ignored files/dirs
        std::fs::write(root.join(".git/objects/ignored.md"), "# Git Ignored\n")
            .expect("write .git file");
        std::fs::write(
            root.join("node_modules/pkg/readme.md"),
            "# Node Modules Ignored\n",
        )
        .expect("write node_modules file");
        std::fs::write(root.join(".hidden-dir/secret.md"), "# Hidden Dir\n")
            .expect("write hidden dir file");
        std::fs::write(root.join(".hidden-file.md"), "# Hidden File\n").expect("write hidden file");
        std::fs::write(root.join("notes.txt"), "Not a markdown file.\n").expect("write txt file");

        let mut conn = open_rw_db(&vault_dir.path().join("vault.db")).expect("open_rw_db");
        let canon = canonicalize_local_dir(root).expect("canonicalize_local_dir");
        let source_id = insert_test_source(&conn, "book", &canon);

        let summary = sync_local_source(&mut conn, source_id, "book", &canon, &[], &[], false)
            .expect("sync_local_source should succeed");

        assert_eq!(summary.source_name, "book");
        assert_eq!(summary.pages_added, 3);
        assert_eq!(summary.pages_updated, 0);
        assert_eq!(summary.pages_unchanged, 0);
        assert_eq!(summary.pages_stale, 0);
        assert_eq!(summary.pages_failed, 0);
        assert_eq!(summary.pruned, 0);

        let uris: Vec<(String, String, String)> = conn
            .prepare("SELECT uri, title, status FROM pages WHERE source_id = ?1 ORDER BY uri")
            .expect("prepare")
            .query_map([source_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(
            uris,
            vec![
                (
                    "README.md".to_string(),
                    "Vault Book".to_string(),
                    "active".to_string()
                ),
                (
                    "chapters/ch01.markdown".to_string(),
                    "Chapter One".to_string(),
                    "active".to_string()
                ),
                (
                    "chapters/nested/deep.MD".to_string(),
                    "Deep Topic".to_string(),
                    "active".to_string()
                ),
            ]
        );

        let section_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections WHERE source_id = ?1",
                [source_id],
                |r| r.get(0),
            )
            .expect("count sections");
        assert_eq!(section_count, 3);

        let fts_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'uppercase'",
                [],
                |r| r.get(0),
            )
            .expect("query sections_fts");
        assert_eq!(fts_hits, 1);

        let run_status: (String, i64) = conn
            .query_row(
                "SELECT status, pages_added FROM sync_runs WHERE source_id = ?1",
                [source_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query sync_runs");
        assert_eq!(run_status, ("ok".to_string(), 3));
    }

    #[test]
    fn resync_skips_unchanged_and_records_diff_on_change() {
        let vault_dir = tempdir().expect("tempdir should succeed");
        let docs_dir = tempdir().expect("docs tempdir should succeed");
        let root = docs_dir.path();

        std::fs::write(
            root.join("alpha.md"),
            "---\ntitle: Alpha\nupdated: 2026-01-01\n---\n\n# Alpha\n\nOriginal alpha section.\n",
        )
        .expect("write alpha");
        std::fs::write(root.join("beta.md"), "# Beta\n\nStable beta body.\n").expect("write beta");

        let mut conn = open_rw_db(&vault_dir.path().join("vault.db")).expect("open_rw_db");
        let canon = canonicalize_local_dir(root).expect("canonicalize_local_dir");
        let source_id = insert_test_source(&conn, "docs", &canon);

        let first = sync_local_source(&mut conn, source_id, "docs", &canon, &[], &[], false)
            .expect("initial sync");
        assert_eq!(first.pages_added, 2);

        // Substantive edit on alpha; beta stays untouched.
        std::fs::write(
            root.join("alpha.md"),
            "---\ntitle: Alpha\nupdated: 2026-11-21\n---\n\n# Alpha\n\nUpdated alpha section.\n\n## New Subsection\n\nTokennewsection content.\n",
        )
        .expect("rewrite alpha");

        let second = sync_local_source(&mut conn, source_id, "docs", &canon, &[], &[], false)
            .expect("second sync");

        assert_eq!(second.pages_added, 0);
        assert_eq!(second.pages_updated, 1);
        assert_eq!(second.pages_unchanged, 1);
        assert_eq!(second.pages_stale, 0);
        assert_eq!(second.pages_failed, 0);

        let diff_row: (String, String) = conn
            .query_row(
                "SELECT p.uri, pr.unified_diff
                 FROM page_revisions pr
                 JOIN pages p ON p.id = pr.page_id
                 WHERE p.source_id = ?1",
                [source_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query page_revisions");
        assert_eq!(diff_row.0, "alpha.md");
        assert!(diff_row.1.contains("-Original alpha section."));
        assert!(diff_row.1.contains("+Updated alpha section."));

        let new_sec_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'tokennewsection'",
                [],
                |r| r.get(0),
            )
            .expect("query new section in FTS");
        assert_eq!(new_sec_hits, 1);

        let old_sec_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'Original'",
                [],
                |r| r.get(0),
            )
            .expect("query old section in FTS");
        assert_eq!(old_sec_hits, 0);
    }

    #[test]
    fn marks_deleted_file_stale_and_prunes_only_with_flag() {
        let vault_dir = tempdir().expect("tempdir should succeed");
        let docs_dir = tempdir().expect("docs tempdir should succeed");
        let root = docs_dir.path();

        std::fs::write(root.join("keep.md"), "# Keep\n\nStays around.\n").expect("write keep");
        std::fs::write(root.join("drop.md"), "# Drop\n\nWill be deleted.\n").expect("write drop");

        let mut conn = open_rw_db(&vault_dir.path().join("vault.db")).expect("open_rw_db");
        let canon = canonicalize_local_dir(root).expect("canonicalize_local_dir");
        let source_id = insert_test_source(&conn, "docs", &canon);

        sync_local_source(&mut conn, source_id, "docs", &canon, &[], &[], false)
            .expect("initial sync");

        std::fs::remove_file(root.join("drop.md")).expect("remove drop.md");

        let default_resync =
            sync_local_source(&mut conn, source_id, "docs", &canon, &[], &[], false)
                .expect("default resync");
        assert_eq!(default_resync.pages_unchanged, 1);
        assert_eq!(default_resync.pages_stale, 1);
        assert_eq!(default_resync.pruned, 0);

        let drop_status: String = conn
            .query_row(
                "SELECT status FROM pages WHERE source_id = ?1 AND uri = 'drop.md'",
                [source_id],
                |r| r.get(0),
            )
            .expect("query drop.md status");
        assert_eq!(drop_status, "stale");

        let prune_resync = sync_local_source(&mut conn, source_id, "docs", &canon, &[], &[], true)
            .expect("prune resync");
        assert_eq!(prune_resync.pages_unchanged, 1);
        assert_eq!(prune_resync.pages_stale, 0);
        assert_eq!(prune_resync.pruned, 1);

        let drop_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages WHERE source_id = ?1 AND uri = 'drop.md'",
                [source_id],
                |r| r.get(0),
            )
            .expect("count drop.md after prune");
        assert_eq!(drop_count, 0);
    }

    #[test]
    fn canonicalizes_path_and_rejects_non_directory() {
        let dir = tempdir().expect("tempdir should succeed");
        let missing = dir.path().join("does-not-exist");
        let file_path = dir.path().join("single-file.md");
        std::fs::write(&file_path, "# Just A File\n").expect("write single file");

        let missing_err = canonicalize_local_dir(&missing).unwrap_err();
        assert!(matches!(missing_err, VaultError::Validation(_)));

        let file_err = canonicalize_local_dir(&file_path).unwrap_err();
        assert!(matches!(file_err, VaultError::Validation(_)));

        let nested = dir.path().join("a").join("..").join("real-docs");
        std::fs::create_dir_all(&nested).expect("create real-docs");
        let canon = canonicalize_local_dir(&nested).expect("canonicalize nested");
        let expected = std::fs::canonicalize(dir.path().join("real-docs")).expect("canonicalize");
        assert_eq!(canon, expected);

        let mut conn = open_rw_db(&dir.path().join("vault.db")).expect("open_rw_db");
        let sync_err = sync_local_source(&mut conn, 1, "bad", &file_path, &[], &[], false)
            .expect_err("sync_local_source must reject non-directory");
        assert!(matches!(sync_err, VaultError::Validation(_)));
    }

    #[test]
    fn filters_files_with_include_and_exclude_patterns() {
        let vault_dir = tempdir().expect("tempdir should succeed");
        let docs_dir = tempdir().expect("docs tempdir should succeed");
        let root = docs_dir.path();

        std::fs::create_dir_all(root.join("docs/drafts")).expect("create dirs");
        std::fs::create_dir_all(root.join("blog")).expect("create blog");
        std::fs::write(root.join("docs/guide.md"), "# Guide\n").expect("write guide");
        std::fs::write(root.join("docs/drafts/wip.md"), "# WIP\n").expect("write wip");
        std::fs::write(root.join("blog/post.md"), "# Post\n").expect("write post");

        let mut conn = open_rw_db(&vault_dir.path().join("vault.db")).expect("open_rw_db");
        let canon = canonicalize_local_dir(root).expect("canonicalize");
        let source_id = insert_test_source(&conn, "filtered", &canon);

        let summary = sync_local_source(
            &mut conn,
            source_id,
            "filtered",
            &canon,
            &["docs/*".to_string()],
            &["docs/drafts".to_string()],
            false,
        )
        .expect("sync with patterns");

        assert_eq!(summary.pages_added, 1);
        let uri: String = conn
            .query_row(
                "SELECT uri FROM pages WHERE source_id = ?1",
                [source_id],
                |r| r.get(0),
            )
            .expect("select uri");
        assert_eq!(uri, "docs/guide.md");
    }
}
