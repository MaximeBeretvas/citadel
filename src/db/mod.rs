pub mod readonly;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

pub use readonly::{
    execute_readonly_query, execute_readonly_query_with_options, QueryOptions, QueryResult,
    DEFAULT_MAX_PAYLOAD_BYTES, DEFAULT_QUERY_LIMIT, DEFAULT_QUERY_TIMEOUT_MS, MAX_QUERY_LIMIT,
};

/// Embedded SQLite DDL, FTS5 triggers, and views.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Open a read-write SQLite connection at `path`, applying WAL pragmas and initializing the schema.
pub fn open_rw_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(path)?;
    apply_rw_pragmas(&conn)?;
    init_db(&conn)?;
    Ok(conn)
}

/// Apply standard read-write pragmas (`WAL`, `NORMAL`, `foreign_keys`, `busy_timeout`).
pub fn apply_rw_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// Apply the embedded schema DDL idempotently to an existing connection.
pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn opens_db_in_wal_mode_with_foreign_keys() {
        let dir = tempdir().expect("tempdir should succeed");
        let db_path = dir.path().join("nested").join("vault.db");

        let conn = open_rw_db(&db_path).expect("open_rw_db should succeed");
        assert!(db_path.exists());

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("PRAGMA journal_mode should return a value");
        assert_eq!(journal_mode.to_lowercase(), "wal");

        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous;", [], |row| row.get(0))
            .expect("PRAGMA synchronous should return a value");
        assert_eq!(synchronous, 1);

        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
            .expect("PRAGMA foreign_keys should return a value");
        assert_eq!(foreign_keys, 1);

        let busy_timeout: i64 = conn
            .query_row("PRAGMA busy_timeout;", [], |row| row.get(0))
            .expect("PRAGMA busy_timeout should return a value");
        assert_eq!(busy_timeout, 5000);

        // Foreign keys must actively reject orphan page inserts.
        let err = conn
            .execute(
                "INSERT INTO pages (source_id, uri, title, content_md, content_hash)
                 VALUES (999, 'missing.md', 'Missing', 'Body', 'abc')",
                [],
            )
            .expect_err("inserting page with invalid source_id should fail");
        assert!(matches!(err, rusqlite::Error::SqliteFailure(_, _)));
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempdir().expect("tempdir should succeed");
        let db_path = dir.path().join("vault.db");

        let conn = open_rw_db(&db_path).expect("initial open_rw_db should succeed");
        conn.execute(
            "INSERT INTO sources (name, kind, origin) VALUES ('rust-docs', 'local', '/tmp/rust-docs')",
            [],
        )
        .expect("insert source should succeed");

        init_db(&conn).expect("second init_db call should succeed");
        drop(conn);

        let reopened = open_rw_db(&db_path).expect("reopening existing db should succeed");
        let count: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE name = 'rust-docs'",
                [],
                |row| row.get(0),
            )
            .expect("query sources should succeed");
        assert_eq!(count, 1);

        let expected_objects = [
            ("table", "sources"),
            ("table", "pages"),
            ("table", "sections"),
            ("table", "page_revisions"),
            ("table", "sync_runs"),
            ("table", "pages_fts"),
            ("table", "sections_fts"),
            ("index", "idx_pages_source_status"),
            ("index", "idx_sections_page"),
            ("index", "idx_sections_source"),
            ("index", "idx_sync_runs_source"),
            ("trigger", "pages_ai"),
            ("trigger", "pages_ad"),
            ("trigger", "pages_au"),
            ("trigger", "sections_ai"),
            ("trigger", "sections_ad"),
            ("trigger", "sections_au"),
            ("view", "v_sources"),
            ("view", "v_recent_changes"),
        ];

        for (obj_type, obj_name) in expected_objects {
            let exists: i64 = reopened
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                    [obj_type, obj_name],
                    |row| row.get(0),
                )
                .expect("query sqlite_master should succeed");
            assert_eq!(exists, 1, "missing {obj_type} `{obj_name}`");
        }
    }

    #[test]
    fn pages_fts_trigger_tracks_insert_update_delete() {
        let dir = tempdir().expect("tempdir should succeed");
        let conn = open_rw_db(&dir.path().join("vault.db")).expect("open_rw_db should succeed");

        conn.execute(
            "INSERT INTO sources (name, kind, origin) VALUES ('docs', 'local', '/tmp/docs')",
            [],
        )
        .expect("insert source");
        let source_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'wal.md', 'SQLite WAL Internals', 'Write-ahead logging enables concurrent readers.', 'h1', 5)",
            [source_id],
        )
        .expect("insert page 1");
        let page1_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'async.md', 'Rust Async', 'Tokio runtime schedules tasks efficiently.', 'h2', 5)",
            [source_id],
        )
        .expect("insert page 2");
        let page2_id = conn.last_insert_rowid();

        let matches_concurrent: Vec<i64> = conn
            .prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH 'concurrent'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(matches_concurrent, vec![page1_id]);

        // Porter stemming check: 'schedule' matches 'schedules'
        let matches_schedule: Vec<i64> = conn
            .prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH 'schedule'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(matches_schedule, vec![page2_id]);

        // Update page 1 FTS-indexed columns
        conn.execute(
            "UPDATE pages SET title = 'SQLite B-Tree', content_md = 'Balanced trees store index keys.' WHERE id = ?1",
            [page1_id],
        )
        .expect("update page 1");

        let old_term_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH 'concurrent'",
                [],
                |row| row.get(0),
            )
            .expect("count old term");
        assert_eq!(old_term_count, 0);

        let new_term_ids: Vec<i64> = conn
            .prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH 'balanced'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(new_term_ids, vec![page1_id]);

        // Update non-FTS column on page 1
        conn.execute(
            "UPDATE pages SET status = 'stale', last_checked_at = '2026-11-21T00:00:00Z' WHERE id = ?1",
            [page1_id],
        )
        .expect("update non-FTS column");

        let still_matches: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH 'balanced'",
                [],
                |row| row.get(0),
            )
            .expect("count after metadata update");
        assert_eq!(still_matches, 1);

        // Upsert via ON CONFLICT DO UPDATE should also fire AFTER UPDATE trigger
        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'wal.md', 'SQLite Upsert', 'Upserted content tokenupsert.', 'h1b', 3)
             ON CONFLICT(source_id, uri) DO UPDATE SET
                title = excluded.title,
                content_md = excluded.content_md,
                content_hash = excluded.content_hash,
                word_count = excluded.word_count",
            [source_id],
        )
        .expect("upsert page 1");

        let upserted_ids: Vec<i64> = conn
            .prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH 'tokenupsert'")
            .expect("prepare upsert query")
            .query_map([], |row| row.get(0))
            .expect("query upsert")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect upsert");
        assert_eq!(upserted_ids, vec![page1_id]);

        // Delete page 1
        conn.execute("DELETE FROM pages WHERE id = ?1", [page1_id])
            .expect("delete page 1");

        let deleted_term_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH 'tokenupsert'",
                [],
                |row| row.get(0),
            )
            .expect("count deleted term");
        assert_eq!(deleted_term_count, 0);

        let remaining_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH 'schedule'",
                [],
                |row| row.get(0),
            )
            .expect("count remaining term");
        assert_eq!(remaining_count, 1);

        conn.execute(
            "INSERT INTO pages_fts(pages_fts) VALUES('integrity-check')",
            [],
        )
        .expect("pages_fts integrity-check should pass");
    }

    #[test]
    fn sections_fts_trigger_tracks_insert_update_delete() {
        let dir = tempdir().expect("tempdir should succeed");
        let conn = open_rw_db(&dir.path().join("vault.db")).expect("open_rw_db should succeed");

        conn.execute(
            "INSERT INTO sources (name, kind, origin) VALUES ('docs', 'local', '/tmp/docs')",
            [],
        )
        .expect("insert source");
        let source_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'guide.md', 'Guide', 'Body', 'h1', 1)",
            [source_id],
        )
        .expect("insert page");
        let page_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
             VALUES (?1, ?2, 0, 'CDC', 'Ingestion > CDC', 2, 'Change data capture streams commits.', 5)",
            [page_id, source_id],
        )
        .expect("insert section 1");
        let sec1_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
             VALUES (?1, ?2, 1, 'Snapshots', 'Ingestion > Snapshots', 2, 'Periodic point-in-time backups.', 4)",
            [page_id, source_id],
        )
        .expect("insert section 2");
        let sec2_id = conn.last_insert_rowid();

        let matches_heading: Vec<i64> = conn
            .prepare("SELECT rowid FROM sections_fts WHERE sections_fts MATCH 'heading_path:CDC'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(matches_heading, vec![sec1_id]);

        // Update section 1
        conn.execute(
            "UPDATE sections SET heading = 'WAL', heading_path = 'Replication > WAL', content_md = 'Logical replication streams frames.' WHERE id = ?1",
            [sec1_id],
        )
        .expect("update section 1");

        let old_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'capture'",
                [],
                |row| row.get(0),
            )
            .expect("count old section term");
        assert_eq!(old_count, 0);

        let new_ids: Vec<i64> = conn
            .prepare("SELECT rowid FROM sections_fts WHERE sections_fts MATCH 'replication'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(new_ids, vec![sec1_id]);

        // Delete section 1
        conn.execute("DELETE FROM sections WHERE id = ?1", [sec1_id])
            .expect("delete section 1");

        let deleted_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'replication'",
                [],
                |row| row.get(0),
            )
            .expect("count deleted section term");
        assert_eq!(deleted_count, 0);

        let sec2_ids: Vec<i64> = conn
            .prepare("SELECT rowid FROM sections_fts WHERE sections_fts MATCH 'backup'")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(sec2_ids, vec![sec2_id]);

        conn.execute(
            "INSERT INTO sections_fts(sections_fts) VALUES('integrity-check')",
            [],
        )
        .expect("sections_fts integrity-check should pass");
    }

    #[test]
    fn v_sources_and_v_recent_changes_return_accurate_counts() {
        let dir = tempdir().expect("tempdir should succeed");
        let conn = open_rw_db(&dir.path().join("vault.db")).expect("open_rw_db should succeed");

        conn.execute(
            "INSERT INTO sources (name, kind, origin, tags, status)
             VALUES ('rust-docs', 'web', 'https://doc.rust-lang.org', '[\"rust\"]', 'ok')",
            [],
        )
        .expect("insert source 1");
        let src1_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sources (name, kind, origin)
             VALUES ('empty-docs', 'local', '/tmp/empty-docs')",
            [],
        )
        .expect("insert source 2");
        let src2_id = conn.last_insert_rowid();

        // Insert pages across active / stale / error states for source 1
        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
             VALUES (?1, 'intro.md', 'Intro', 'Alpha uniquealpha', 'h1', 120, 'active')",
            [src1_id],
        )
        .expect("insert page A");
        let page_a_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
             VALUES (?1, 'guide.md', 'Guide', 'Beta uniquebeta', 'h2', 80, 'active')",
            [src1_id],
        )
        .expect("insert page B");
        let page_b_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
             VALUES (?1, 'old.md', 'Old', 'Gamma', 'h3', 50, 'stale')",
            [src1_id],
        )
        .expect("insert page C");

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count, status)
             VALUES (?1, 'broken.md', 'Broken', 'Delta', 'h4', 30, 'error')",
            [src1_id],
        )
        .expect("insert page D");

        // Sections for source 1
        conn.execute(
            "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
             VALUES (?1, ?2, 0, 'Overview', 'Intro > Overview', 1, 'Contains sectiontoken.', 2)",
            [page_a_id, src1_id],
        )
        .expect("insert section");

        // Revisions for source 1
        conn.execute(
            "INSERT INTO page_revisions (page_id, previous_hash, new_hash, unified_diff, changed_at)
             VALUES (?1, 'old1', 'h1', '@@ -1 +1 @@\n-old\n+new', '2026-11-20T10:00:00Z')",
            [page_a_id],
        )
        .expect("insert revision A");
        conn.execute(
            "INSERT INTO page_revisions (page_id, previous_hash, new_hash, unified_diff, changed_at)
             VALUES (?1, 'old2', 'h2', '@@ -1 +1 @@\n-foo\n+bar', '2026-11-21T12:00:00Z')",
            [page_b_id],
        )
        .expect("insert revision B");

        // Sync run for source 1
        conn.execute(
            "INSERT INTO sync_runs (source_id, started_at, finished_at, status, pages_added)
             VALUES (?1, '2026-11-21T12:00:00Z', '2026-11-21T12:00:02Z', 'ok', 4)",
            [src1_id],
        )
        .expect("insert sync_run");

        // Verify v_sources counts
        type SourceRow = (i64, String, i64, i64, i64, i64);
        let source_rows: Vec<SourceRow> = conn
            .prepare(
                "SELECT id, name, active_pages, stale_pages, error_pages, total_words
                 FROM v_sources ORDER BY id",
            )
            .expect("prepare v_sources")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .expect("query v_sources")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect v_sources");

        assert_eq!(
            source_rows,
            vec![
                (src1_id, "rust-docs".to_string(), 2, 1, 1, 280),
                (src2_id, "empty-docs".to_string(), 0, 0, 0, 0),
            ]
        );

        let (max_pages, max_depth, render_js, refresh_interval_secs): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT max_pages, max_depth, render_js, refresh_interval_secs
                 FROM v_sources WHERE id = ?1",
                [src1_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query extended columns on v_sources");
        assert_eq!(
            (max_pages, max_depth, render_js, refresh_interval_secs),
            (500, 5, 0, 86400)
        );

        // Verify v_recent_changes order and columns
        type ChangeRow = (
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let changes: Vec<ChangeRow> = conn
            .prepare(
                "SELECT page_id, source_id, source_name, uri, title, previous_hash, new_hash, unified_diff, changed_at
                 FROM v_recent_changes",
            )
            .expect("prepare v_recent_changes")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })
            .expect("query v_recent_changes")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect v_recent_changes");

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].0, page_b_id);
        assert_eq!(changes[0].1, src1_id);
        assert_eq!(changes[0].2, "rust-docs");
        assert_eq!(changes[0].3, "guide.md");
        assert_eq!(changes[0].4, "Guide");
        assert_eq!(changes[0].5, "old2");
        assert_eq!(changes[0].6, "h2");
        assert_eq!(changes[0].8, "2026-11-21T12:00:00Z");

        assert_eq!(changes[1].0, page_a_id);
        assert_eq!(changes[1].8, "2026-11-20T10:00:00Z");

        // Deleting source 1 must cascade to pages, sections, page_revisions, sync_runs, AND FTS indexes
        conn.execute("DELETE FROM sources WHERE id = ?1", [src1_id])
            .expect("delete source 1");

        let remaining_pages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages WHERE source_id = ?1",
                [src1_id],
                |r| r.get(0),
            )
            .expect("count pages");
        let remaining_sections: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections WHERE source_id = ?1",
                [src1_id],
                |r| r.get(0),
            )
            .expect("count sections");
        let remaining_revisions: i64 = conn
            .query_row("SELECT COUNT(*) FROM page_revisions", [], |r| r.get(0))
            .expect("count page_revisions");
        let remaining_sync_runs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_runs WHERE source_id = ?1",
                [src1_id],
                |r| r.get(0),
            )
            .expect("count sync_runs");

        assert_eq!(remaining_pages, 0);
        assert_eq!(remaining_sections, 0);
        assert_eq!(remaining_revisions, 0);
        assert_eq!(remaining_sync_runs, 0);

        let fts_pages_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH 'uniquealpha'",
                [],
                |r| r.get(0),
            )
            .expect("query pages_fts after cascade");
        let fts_sections_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sections_fts WHERE sections_fts MATCH 'sectiontoken'",
                [],
                |r| r.get(0),
            )
            .expect("query sections_fts after cascade");

        assert_eq!(fts_pages_count, 0);
        assert_eq!(fts_sections_count, 0);

        conn.execute(
            "INSERT INTO pages_fts(pages_fts) VALUES('integrity-check')",
            [],
        )
        .expect("pages_fts integrity-check after cascade should pass");
        conn.execute(
            "INSERT INTO sections_fts(sections_fts) VALUES('integrity-check')",
            [],
        )
        .expect("sections_fts integrity-check after cascade should pass");
    }
}
