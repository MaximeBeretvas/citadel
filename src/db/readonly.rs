use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::ffi::ErrorCode;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::error::{Result, VaultError};

/// Default maximum number of rows returned by read-only queries.
pub const DEFAULT_QUERY_LIMIT: usize = 50;

/// Hard maximum number of rows returned by read-only queries.
pub const MAX_QUERY_LIMIT: usize = 200;

/// Default query execution timeout in milliseconds.
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 5000;

/// Default serialized JSON payload cap in bytes (100 KB).
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 100 * 1024;

const ROW_TRUNCATION_HINT: &str =
    "Result truncated at row limit; refine your query or increase limit (max 200).";

const PAYLOAD_TRUNCATION_HINT: &str =
    "Response exceeded 100 KB payload cap; select sections.content_md or use snippet() instead of full pages.content_md.";

const TRUNCATED_CELL_SUFFIX: &str = "... [truncated]";

/// Structured output returned by read-only SQLite queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_hint: Option<String>,
    pub elapsed_ms: u64,
}

/// Configuration options controlling limits and caps for read-only queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOptions {
    pub limit: Option<usize>,
    pub timeout: Duration,
    pub max_payload_bytes: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            limit: None,
            timeout: Duration::from_millis(DEFAULT_QUERY_TIMEOUT_MS),
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
        }
    }
}

/// Execute a single read-only SQL `SELECT` query against `db_path` using default timeout and payload caps.
pub fn execute_readonly_query(
    db_path: &Path,
    sql: &str,
    limit: Option<usize>,
) -> Result<QueryResult> {
    execute_readonly_query_with_options(
        db_path,
        sql,
        QueryOptions {
            limit,
            ..Default::default()
        },
    )
}

/// Execute a single read-only SQL `SELECT` query against `db_path` with explicit resource options.
pub fn execute_readonly_query_with_options(
    db_path: &Path,
    sql: &str,
    options: QueryOptions,
) -> Result<QueryResult> {
    let effective_limit = validate_options(&options)?;
    let clean_sql = extract_single_statement(sql)?;

    if !db_path.exists() {
        return Err(VaultError::NotFound(format!(
            "vault database not found at {}",
            db_path.display()
        )));
    }

    // Layer 2: OS/SQLite read-only mode + read-only pragmas before installing the authorizer.
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA busy_timeout = 3000; PRAGMA query_only = ON;")?;

    // Layer 3: SQLite authorizer allowlist.
    let denied_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let denied_slot = Arc::clone(&denied_reason);
    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        match authorize_action(ctx) {
            Ok(()) => Authorization::Allow,
            Err(reason) => {
                if let Ok(mut guard) = denied_slot.lock() {
                    if guard.is_none() {
                        *guard = Some(reason);
                    }
                }
                Authorization::Deny
            }
        }
    }))?;

    // Layer 4a: Execution timeout via SQLite VM progress handler.
    let started = Instant::now();
    let timeout = options.timeout;
    conn.progress_handler(1000, Some(move || started.elapsed() >= timeout))?;

    let mut stmt = match conn.prepare(clean_sql) {
        Ok(stmt) => stmt,
        Err(err) => return Err(map_query_error(err, &denied_reason, timeout)),
    };

    if !stmt.readonly()
        || stmt.is_explain() != 0
        || stmt.column_count() == 0
        || !starts_with_select_keyword(clean_sql)
    {
        return Err(VaultError::InvalidQuery(
            "only read-only SQL SELECT statements are allowed".into(),
        ));
    }

    let columns: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    let col_count = columns.len();

    // Reserve exact worst-case JSON envelope bytes so serialized output never exceeds max_payload_bytes.
    let envelope_bytes = compute_envelope_bytes(&columns, effective_limit)?;
    let row_budget = options.max_payload_bytes.saturating_sub(envelope_bytes);

    let mut rows_out: Vec<Vec<Value>> = Vec::with_capacity(effective_limit.min(64));
    let mut accumulated_rows_bytes: usize = 0;
    let mut truncated = false;
    let mut truncation_hint: Option<String> = None;

    let mut rows = match stmt.query([]) {
        Ok(rows) => rows,
        Err(err) => return Err(map_query_error(err, &denied_reason, timeout)),
    };

    loop {
        let next_row = match rows.next() {
            Ok(row) => row,
            Err(err) => return Err(map_query_error(err, &denied_reason, timeout)),
        };

        let Some(row) = next_row else {
            break;
        };

        // Layer 4b: Row cap (probe 1 row past effective_limit to report `truncated` accurately).
        if rows_out.len() >= effective_limit {
            truncated = true;
            truncation_hint = Some(ROW_TRUNCATION_HINT.to_string());
            break;
        }

        let mut values = row_to_json_values(row, col_count)?;
        let row_bytes = serialize_json_len(&values)?;
        let separator_bytes = usize::from(!rows_out.is_empty());
        let next_total = accumulated_rows_bytes
            .saturating_add(separator_bytes)
            .saturating_add(row_bytes);

        // Layer 4c: 100 KB serialized JSON payload cap.
        if next_total > row_budget {
            if rows_out.is_empty() && fit_row_to_budget(&mut values, row_budget)? {
                rows_out.push(values);
            }
            truncated = true;
            truncation_hint = Some(PAYLOAD_TRUNCATION_HINT.to_string());
            break;
        }

        accumulated_rows_bytes = next_total;
        rows_out.push(values);
    }

    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let row_count = rows_out.len();

    Ok(QueryResult {
        columns,
        rows: rows_out,
        row_count,
        truncated,
        truncation_hint,
        elapsed_ms,
    })
}

fn validate_options(options: &QueryOptions) -> Result<usize> {
    if options.limit == Some(0) {
        return Err(VaultError::InvalidQuery(
            "limit must be between 1 and 200".into(),
        ));
    }
    if options.max_payload_bytes == 0 {
        return Err(VaultError::InvalidQuery(
            "max_payload_bytes must be greater than 0".into(),
        ));
    }
    Ok(options
        .limit
        .unwrap_or(DEFAULT_QUERY_LIMIT)
        .min(MAX_QUERY_LIMIT))
}

/// Layer 1: Single-statement SQL scanner respecting string literals, quoted/bracketed identifiers, and comments.
fn extract_single_statement(sql: &str) -> Result<&str> {
    let bytes = sql.as_bytes();
    let stmt_start = skip_whitespace_and_comments(bytes, 0)?;
    if stmt_start == bytes.len() {
        return Err(VaultError::InvalidQuery(
            "SQL query must not be empty".into(),
        ));
    }
    if bytes[stmt_start] == b';' {
        return Err(VaultError::InvalidQuery(
            "only a single non-empty SQL SELECT statement is allowed".into(),
        ));
    }

    let mut i = stmt_start;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => i = scan_escaped_quote(bytes, i, b'\''),
            b'"' => i = scan_escaped_quote(bytes, i, b'"'),
            b'`' => i = scan_escaped_quote(bytes, i, b'`'),
            b'[' => i = scan_bracket_ident(bytes, i),
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i = skip_line_comment(bytes, i);
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i = skip_block_comment(bytes, i)?;
            }
            b';' => {
                let stmt_end = i;
                let after = skip_whitespace_and_comments(bytes, i + 1)?;
                if after < bytes.len() {
                    return Err(VaultError::InvalidQuery(
                        "only a single SQL SELECT statement is allowed; multiple statements are rejected"
                            .into(),
                    ));
                }
                return Ok(sql[stmt_start..stmt_end].trim());
            }
            _ => i += 1,
        }
    }

    Ok(sql[stmt_start..].trim())
}

fn skip_whitespace_and_comments(bytes: &[u8], mut i: usize) -> Result<usize> {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i = skip_line_comment(bytes, i);
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i = skip_block_comment(bytes, i)?;
            continue;
        }
        return Ok(i);
    }
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() {
        i + 1
    } else {
        i
    }
}

fn skip_block_comment(bytes: &[u8], start: usize) -> Result<usize> {
    let mut i = start + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Ok(i + 2);
        }
        i += 1;
    }
    Err(VaultError::InvalidQuery(
        "unterminated block comment in SQL query".into(),
    ))
}

fn scan_escaped_quote(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if i + 1 < bytes.len() && bytes[i + 1] == quote {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn scan_bracket_ident(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b']' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn starts_with_select_keyword(clean_sql: &str) -> bool {
    let bytes = clean_sql.as_bytes();
    let Ok(mut i) = skip_whitespace_and_comments(bytes, 0) else {
        return false;
    };
    while i < bytes.len() && bytes[i] == b'(' {
        i += 1;
        let Ok(next) = skip_whitespace_and_comments(bytes, i) else {
            return false;
        };
        i = next;
    }

    let rest = &bytes[i..];
    ["SELECT", "WITH", "VALUES"].iter().any(|kw| {
        let kw_bytes = kw.as_bytes();
        rest.len() >= kw_bytes.len()
            && rest[..kw_bytes.len()].eq_ignore_ascii_case(kw_bytes)
            && rest
                .get(kw_bytes.len())
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
    })
}

fn authorize_action(ctx: AuthContext<'_>) -> std::result::Result<(), String> {
    match ctx.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => Ok(()),
        AuthAction::Function { function_name } => {
            if is_blocked_function(function_name) {
                Err(format!("function `{function_name}`"))
            } else {
                Ok(())
            }
        }
        AuthAction::Insert { table_name } => Err(format!("INSERT on `{table_name}`")),
        AuthAction::Update { table_name, .. } => Err(format!("UPDATE on `{table_name}`")),
        AuthAction::Delete { table_name } => Err(format!("DELETE on `{table_name}`")),
        AuthAction::Pragma {
            pragma_name,
            pragma_value: None,
        } if pragma_name.eq_ignore_ascii_case("data_version") => {
            // SQLite's FTS5 virtual table executes `PRAGMA data_version` internally on read queries;
            // top-level `PRAGMA` statements are still blocked by `starts_with_select_keyword`.
            Ok(())
        }
        AuthAction::Pragma { pragma_name, .. } => Err(format!("PRAGMA `{pragma_name}`")),
        AuthAction::Attach { .. } => Err("ATTACH DATABASE".to_string()),
        AuthAction::Detach { database_name } => Err(format!("DETACH `{database_name}`")),
        AuthAction::Transaction { .. } => Err("TRANSACTION".to_string()),
        AuthAction::Savepoint { .. } => Err("SAVEPOINT".to_string()),
        other => Err(format!("{other:?}")),
    }
}

fn is_blocked_function(function_name: &str) -> bool {
    function_name.eq_ignore_ascii_case("load_extension")
        || function_name.eq_ignore_ascii_case("fts3_tokenizer")
        || function_name.eq_ignore_ascii_case("writefile")
        || function_name.eq_ignore_ascii_case("edit")
}

fn map_query_error(
    err: rusqlite::Error,
    denied_reason: &Arc<Mutex<Option<String>>>,
    timeout: Duration,
) -> VaultError {
    match err.sqlite_error_code() {
        Some(ErrorCode::OperationInterrupted) => {
            VaultError::QueryTimeout(format!("query exceeded {} ms timeout", timeout.as_millis()))
        }
        Some(ErrorCode::AuthorizationForStatementDenied) => {
            let reason = denied_reason
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .unwrap_or_else(|| "unauthorized SQL operation".to_string());
            VaultError::InvalidQuery(format!(
                "write or restricted SQL operation ({reason}) is disabled on read-only queries"
            ))
        }
        Some(ErrorCode::ReadOnly) => {
            VaultError::InvalidQuery("write operations are disabled on read-only queries".into())
        }
        Some(ErrorCode::Unknown) => VaultError::InvalidQuery(err.to_string()),
        _ => match err {
            rusqlite::Error::SqlInputError { msg, .. } => VaultError::InvalidQuery(msg),
            other => VaultError::Sqlite(other),
        },
    }
}

fn row_to_json_values(row: &rusqlite::Row<'_>, col_count: usize) -> Result<Vec<Value>> {
    let mut values = Vec::with_capacity(col_count);
    for idx in 0..col_count {
        let val = match row.get_ref(idx)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::Number(i.into()),
            ValueRef::Real(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
            ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => Value::String(encode_hex_blob(bytes)),
        };
        values.push(val);
    }
    Ok(values)
}

fn encode_hex_blob(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn compute_envelope_bytes(columns: &[String], max_rows: usize) -> Result<usize> {
    let longest_hint = if PAYLOAD_TRUNCATION_HINT.len() >= ROW_TRUNCATION_HINT.len() {
        PAYLOAD_TRUNCATION_HINT
    } else {
        ROW_TRUNCATION_HINT
    };

    let worst_case = QueryResult {
        columns: columns.to_vec(),
        rows: Vec::new(),
        row_count: max_rows.max(1),
        truncated: true,
        truncation_hint: Some(longest_hint.to_string()),
        elapsed_ms: u64::MAX,
    };
    serialize_json_len(&worst_case)
}

fn serialize_json_len<T: Serialize + ?Sized>(value: &T) -> Result<usize> {
    struct ByteCounter(usize);

    impl std::io::Write for ByteCounter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|err| VaultError::Other(format!("failed to serialize query payload: {err}")))?;
    Ok(counter.0)
}

/// Shrink oversized string cells in a single huge first row so it still stays within `budget`.
fn fit_row_to_budget(values: &mut [Value], budget: usize) -> Result<bool> {
    if serialize_json_len(values)? <= budget {
        return Ok(true);
    }

    loop {
        let Some((idx, original)) = find_longest_shrinkable_string(values) else {
            return Ok(false);
        };

        // First check if replacing this entire string with the suffix makes the row fit.
        values[idx] = Value::String(TRUNCATED_CELL_SUFFIX.to_string());
        if serialize_json_len(values)? > budget {
            // Still over budget due to other columns; leave this cell minimal and shrink the next column.
            continue;
        }

        // Binary-search the longest UTF-8 prefix + suffix that fits within `budget`.
        let mut lo = 0usize;
        let mut hi = original.len();
        let mut best_prefix_end = 0usize;

        while lo <= hi {
            let mid = floor_char_boundary(&original, lo + (hi - lo) / 2);
            let mut candidate = String::with_capacity(mid + TRUNCATED_CELL_SUFFIX.len());
            candidate.push_str(&original[..mid]);
            candidate.push_str(TRUNCATED_CELL_SUFFIX);
            values[idx] = Value::String(candidate);

            if serialize_json_len(values)? <= budget {
                best_prefix_end = mid;
                match next_char_boundary(&original, mid) {
                    Some(next) => lo = next,
                    None => break,
                }
            } else if mid == 0 {
                break;
            } else {
                hi = prev_char_boundary(&original, mid);
            }
        }

        let mut final_str = String::with_capacity(best_prefix_end + TRUNCATED_CELL_SUFFIX.len());
        final_str.push_str(&original[..best_prefix_end]);
        final_str.push_str(TRUNCATED_CELL_SUFFIX);
        values[idx] = Value::String(final_str);
        return Ok(serialize_json_len(values)? <= budget);
    }
}

fn find_longest_shrinkable_string(values: &[Value]) -> Option<(usize, String)> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, val) in values.iter().enumerate() {
        if let Value::String(s) = val {
            if s.len() > TRUNCATED_CELL_SUFFIX.len()
                && best.is_none_or(|(_, best_len)| s.len() > best_len)
            {
                best = Some((idx, s.len()));
            }
        }
    }
    best.and_then(|(idx, _)| match &values[idx] {
        Value::String(s) => Some((idx, s.clone())),
        _ => None,
    })
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    floor_char_boundary(s, idx.saturating_sub(1))
}

fn next_char_boundary(s: &str, idx: usize) -> Option<usize> {
    if idx >= s.len() {
        return None;
    }
    let mut next = idx + 1;
    while next < s.len() && !s.is_char_boundary(next) {
        next += 1;
    }
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_rw_db;
    use std::time::Instant;
    use tempfile::{tempdir, TempDir};

    fn setup_test_vault() -> (TempDir, std::path::PathBuf) {
        let dir = tempdir().expect("tempdir should succeed");
        let db_path = dir.path().join("vault.db");
        let conn = open_rw_db(&db_path).expect("open_rw_db should succeed");

        conn.execute(
            "INSERT INTO sources (name, kind, origin, status)
             VALUES ('rust-docs', 'local', '/tmp/rust-docs', 'ok')",
            [],
        )
        .expect("insert source");
        let source_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'wal.md', 'SQLite WAL Guide', 'Write-ahead logging allows concurrent readers and writers.', 'hash1', 8)",
            [source_id],
        )
        .expect("insert page");
        let page_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
             VALUES (?1, ?2, 0, 'Concurrency', 'WAL > Concurrency', 2, 'Write-ahead logging allows concurrent readers without blocking writers.', 9)",
            [page_id, source_id],
        )
        .expect("insert section 1");

        conn.execute(
            "INSERT INTO sections (page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
             VALUES (?1, ?2, 1, 'Checkpointing', 'WAL > Checkpointing', 2, 'Checkpoints move WAL frames back into the database file.', 9)",
            [page_id, source_id],
        )
        .expect("insert section 2");

        (dir, db_path)
    }

    #[test]
    fn allows_select_and_fts5_bm25_snippet() {
        let (_dir, db_path) = setup_test_vault();

        let sql = "
            SELECT
                src.name AS source,
                p.uri,
                s.heading_path,
                bm25(sections_fts) AS rank,
                snippet(sections_fts, 1, '[', ']', '...', 12) AS excerpt,
                highlight(sections_fts, 1, '<b>', '</b>') AS highlighted,
                NULL AS optional_note,
                s.ordinal
            FROM sections_fts
            JOIN sections s ON s.id = sections_fts.rowid
            JOIN pages p ON p.id = s.page_id
            JOIN sources src ON src.id = s.source_id
            WHERE sections_fts MATCH 'concurrent'
            ORDER BY rank
        ";

        let result = execute_readonly_query(&db_path, sql, None)
            .expect("valid FTS5 SELECT query should succeed");

        assert_eq!(
            result.columns,
            vec![
                "source",
                "uri",
                "heading_path",
                "rank",
                "excerpt",
                "highlighted",
                "optional_note",
                "ordinal",
            ]
        );
        assert_eq!(result.row_count, 1);
        assert_eq!(result.rows.len(), 1);
        assert!(!result.truncated);
        assert!(result.truncation_hint.is_none());
        assert!(result.elapsed_ms < 5000);

        let row = &result.rows[0];
        assert_eq!(row[0], Value::String("rust-docs".to_string()));
        assert_eq!(row[1], Value::String("wal.md".to_string()));
        assert_eq!(row[2], Value::String("WAL > Concurrency".to_string()));
        assert!(row[3].is_f64() || row[3].is_number());
        let excerpt = row[4].as_str().expect("excerpt should be string");
        assert!(excerpt.contains("[concurrent]"));
        let highlighted = row[5].as_str().expect("highlighted should be string");
        assert!(highlighted.contains("<b>concurrent</b>"));
        assert_eq!(row[6], Value::Null);
        assert_eq!(row[7], Value::from(0));
    }

    #[test]
    fn blocks_all_write_and_ddl_statements() {
        let (_dir, db_path) = setup_test_vault();

        let disallowed_queries = [
            "INSERT INTO sources (name, kind, origin) VALUES ('evil', 'local', '/tmp/evil')",
            "UPDATE sources SET name = 'hacked'",
            "DELETE FROM pages",
            "DROP TABLE sources",
            "ALTER TABLE sources ADD COLUMN hacked TEXT",
            "CREATE TABLE hacked (id INTEGER PRIMARY KEY)",
            "CREATE TEMP TABLE temp_hacked (id INTEGER)",
            "REPLACE INTO sources (id, name, kind, origin) VALUES (1, 'x', 'local', '/tmp/x')",
            "VACUUM",
            "BEGIN TRANSACTION",
            "REINDEX",
            "ANALYZE",
            "EXPLAIN SELECT * FROM sources",
            "SELECT load_extension('/tmp/evil.so')",
        ];

        for sql in disallowed_queries {
            let err = execute_readonly_query(&db_path, sql, None).unwrap_err();
            assert!(
                matches!(err, VaultError::InvalidQuery(_)),
                "expected InvalidQuery for `{sql}`, got: {err:?}"
            );
        }

        // Verify original data remained completely intact.
        let check = execute_readonly_query(&db_path, "SELECT COUNT(*) AS n FROM sources", None)
            .expect("count query should succeed");
        assert_eq!(check.rows, vec![vec![Value::from(1)]]);
    }

    #[test]
    fn blocks_attach_detach_and_pragma() {
        let (dir, db_path) = setup_test_vault();
        let other_db = dir.path().join("other.db");
        let _other = open_rw_db(&other_db).expect("create other db");

        let attach_sql = format!("ATTACH DATABASE '{}' AS other", other_db.display());
        let disallowed_queries = [
            attach_sql.as_str(),
            "DETACH DATABASE main",
            "PRAGMA query_only = OFF",
            "PRAGMA writable_schema = ON",
            "PRAGMA user_version = 42",
            "PRAGMA table_info(sources)",
            "PRAGMA data_version",
        ];

        for sql in disallowed_queries {
            let err = execute_readonly_query(&db_path, sql, None).unwrap_err();
            assert!(
                matches!(err, VaultError::InvalidQuery(_)),
                "expected InvalidQuery for `{sql}`, got: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_multi_statement_sql() {
        let (_dir, db_path) = setup_test_vault();

        let invalid_queries = [
            "",
            "   \n\t  ",
            "-- only a comment",
            "/* block comment only */",
            "; SELECT 1",
            "SELECT 1;;",
            "SELECT 1; SELECT 2",
            "SELECT 1; DROP TABLE sources",
            "SELECT 1; /* comment */ SELECT 2",
        ];

        for sql in invalid_queries {
            let err = execute_readonly_query(&db_path, sql, None).unwrap_err();
            assert!(
                matches!(err, VaultError::InvalidQuery(_)),
                "expected InvalidQuery for `{sql}`, got: {err:?}"
            );
        }

        // Semicolons inside string literals/comments and a single trailing semicolon MUST be allowed.
        let valid_with_literals = execute_readonly_query(
            &db_path,
            "SELECT 'hello;world' AS txt, \"col;name\" AS q FROM (SELECT 1 AS \"col;name\"); -- trailing",
            None,
        )
        .expect("single statement with semicolons in literals/comments should succeed");
        assert_eq!(valid_with_literals.row_count, 1);
        assert_eq!(
            valid_with_literals.rows[0],
            vec![Value::String("hello;world".to_string()), Value::from(1)]
        );
    }

    #[test]
    fn enforces_row_limit_and_truncation_flag() {
        let (_dir, db_path) = setup_test_vault();

        // Exactly 250 generated rows via CTE (does not mutate DB).
        let sql_250 = "
            WITH RECURSIVE seq(n) AS (
                SELECT 1
                UNION ALL
                SELECT n + 1 FROM seq WHERE n < 250
            )
            SELECT n FROM seq
        ";

        // Default limit = 50 -> truncated = true
        let default_res =
            execute_readonly_query(&db_path, sql_250, None).expect("default limit query");
        assert_eq!(default_res.row_count, 50);
        assert_eq!(default_res.rows.len(), 50);
        assert!(default_res.truncated);
        assert!(default_res.truncation_hint.is_some());

        // Limit above 200 is clamped to hard max 200 -> truncated = true
        let clamped_res =
            execute_readonly_query(&db_path, sql_250, Some(500)).expect("clamped max limit query");
        assert_eq!(clamped_res.row_count, 200);
        assert_eq!(clamped_res.rows.len(), 200);
        assert!(clamped_res.truncated);

        // Exact boundary: 200 rows with limit 200 -> truncated = false
        let sql_200 = "
            WITH RECURSIVE seq(n) AS (
                SELECT 1
                UNION ALL
                SELECT n + 1 FROM seq WHERE n < 200
            )
            SELECT n FROM seq
        ";
        let exact_res =
            execute_readonly_query(&db_path, sql_200, Some(200)).expect("exact boundary query");
        assert_eq!(exact_res.row_count, 200);
        assert!(!exact_res.truncated);
        assert!(exact_res.truncation_hint.is_none());

        // Zero limit is rejected cleanly
        let zero_err = execute_readonly_query(&db_path, "SELECT 1", Some(0)).unwrap_err();
        assert!(matches!(zero_err, VaultError::InvalidQuery(_)));
    }

    #[test]
    fn enforces_100kb_payload_cap() {
        let dir = tempdir().expect("tempdir should succeed");
        let db_path = dir.path().join("vault.db");
        let conn = open_rw_db(&db_path).expect("open_rw_db should succeed");

        conn.execute(
            "INSERT INTO sources (name, kind, origin) VALUES ('big', 'local', '/tmp/big')",
            [],
        )
        .expect("insert source");
        let source_id = conn.last_insert_rowid();

        let chunk_25kb = "a".repeat(25 * 1024);
        for i in 0..10 {
            conn.execute(
                "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, 100)",
                rusqlite::params![
                    source_id,
                    format!("page-{i}.md"),
                    format!("Page {i}"),
                    chunk_25kb,
                    format!("hash-{i}"),
                ],
            )
            .expect("insert large page");
        }

        // Multi-row payload exceeds 100 KB -> stops cleanly before exceeding cap
        let multi_res = execute_readonly_query(
            &db_path,
            "SELECT uri, content_md FROM pages ORDER BY id",
            Some(50),
        )
        .expect("large multi-row query should truncate cleanly");

        assert!(multi_res.truncated);
        assert!(multi_res.row_count >= 1 && multi_res.row_count < 10);
        let hint = multi_res
            .truncation_hint
            .as_deref()
            .expect("truncation_hint should explain 100 KB cap");
        assert!(hint.contains("sections") && hint.contains("snippet()"));
        let serialized_multi = serde_json::to_vec(&multi_res).expect("serialize result");
        assert!(
            serialized_multi.len() <= DEFAULT_MAX_PAYLOAD_BYTES,
            "serialized size {} exceeded 100 KB cap",
            serialized_multi.len()
        );

        // Single giant row (>100 KB in one cell, including multibyte UTF-8) -> stays within 100 KB cap
        let huge_150kb = "é".repeat(75 * 1024);
        conn.execute(
            "INSERT INTO pages (source_id, uri, title, content_md, content_hash, word_count)
             VALUES (?1, 'giant.md', 'Giant', ?2, 'hash-giant', 100)",
            rusqlite::params![source_id, huge_150kb],
        )
        .expect("insert giant page");

        let single_res = execute_readonly_query(
            &db_path,
            "SELECT uri, content_md FROM pages WHERE uri = 'giant.md'",
            Some(10),
        )
        .expect("single huge row should truncate cleanly");
        assert!(single_res.truncated);
        assert_eq!(single_res.row_count, 1);
        let single_hint = single_res
            .truncation_hint
            .as_deref()
            .expect("truncation_hint should be present for single huge row");
        assert!(single_hint.contains("sections") && single_hint.contains("snippet()"));
        let cell_text = single_res.rows[0][1]
            .as_str()
            .expect("truncated content_md should remain a valid string");
        assert!(cell_text.ends_with(TRUNCATED_CELL_SUFFIX));
        let serialized_single = serde_json::to_vec(&single_res).expect("serialize single result");
        assert!(
            serialized_single.len() <= DEFAULT_MAX_PAYLOAD_BYTES,
            "serialized single-row size {} exceeded 100 KB cap",
            serialized_single.len()
        );
    }

    #[test]
    fn aborts_slow_query_on_timeout() {
        let (_dir, db_path) = setup_test_vault();

        let slow_sql = "
            WITH RECURSIVE cnt(x) AS (
                SELECT 1
                UNION ALL
                SELECT x + 1 FROM cnt WHERE x < 100000000
            )
            SELECT COUNT(*) FROM cnt
        ";

        let started = Instant::now();
        let err = execute_readonly_query_with_options(
            &db_path,
            slow_sql,
            QueryOptions {
                limit: Some(50),
                timeout: Duration::from_millis(100),
                max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            },
        )
        .unwrap_err();

        let elapsed = started.elapsed();
        assert!(
            matches!(err, VaultError::QueryTimeout(_)),
            "expected QueryTimeout, got: {err:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout took too long: {elapsed:?}"
        );
    }
}
