CREATE TABLE IF NOT EXISTS sources (
    id                    INTEGER PRIMARY KEY,
    name                  TEXT NOT NULL UNIQUE,           -- slug, e.g. 'hex-docs'
    kind                  TEXT NOT NULL CHECK (kind IN ('web', 'local')),
    origin                TEXT NOT NULL UNIQUE,           -- root URL or absolute dir path
    tags                  TEXT NOT NULL DEFAULT '[]',     -- JSON array of strings
    include_patterns      TEXT NOT NULL DEFAULT '[]',     -- JSON glob/prefix list
    exclude_patterns      TEXT NOT NULL DEFAULT '[]',     -- JSON glob/prefix list
    max_pages             INTEGER NOT NULL DEFAULT 500,
    max_depth             INTEGER NOT NULL DEFAULT 5,
    render_js             INTEGER NOT NULL DEFAULT 0,     -- 1 = use headless Chrome if enabled
    refresh_interval_secs INTEGER NOT NULL DEFAULT 86400, -- 0 = manual only
    status                TEXT NOT NULL DEFAULT 'idle'
                          CHECK (status IN ('idle', 'syncing', 'ok', 'error')),
    last_synced_at        TEXT,                           -- ISO-8601 UTC
    last_error            TEXT,
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS pages (
    id               INTEGER PRIMARY KEY,
    source_id        INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    uri              TEXT NOT NULL,                       -- canonical URL or relative file path
    title            TEXT NOT NULL DEFAULT '',
    content_md       TEXT NOT NULL,                       -- clean Markdown body
    content_hash     TEXT NOT NULL,                       -- SHA-256 of normalized content
    word_count       INTEGER NOT NULL DEFAULT 0,
    etag             TEXT,                                -- HTTP ETag for conditional GET
    last_modified    TEXT,                                -- HTTP Last-Modified header
    status           TEXT NOT NULL DEFAULT 'active'
                     CHECK (status IN ('active', 'stale', 'error')),
    http_status      INTEGER,
    error_message    TEXT,
    first_seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    last_checked_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    last_changed_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (source_id, uri)
);

CREATE TABLE IF NOT EXISTS sections (
    id            INTEGER PRIMARY KEY,
    page_id       INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    ordinal       INTEGER NOT NULL,                       -- 0-based order in page
    heading       TEXT NOT NULL DEFAULT '',               -- section heading text
    heading_path  TEXT NOT NULL DEFAULT '',               -- breadcrumb: 'Ingestion > CDC'
    level         INTEGER NOT NULL DEFAULT 0,             -- 0=intro, 1=H1, 2=H2, 3=H3
    content_md    TEXT NOT NULL,
    word_count    INTEGER NOT NULL DEFAULT 0
);

-- Keeps the most recent diff per page (upserted on content change)
CREATE TABLE IF NOT EXISTS page_revisions (
    page_id        INTEGER PRIMARY KEY REFERENCES pages(id) ON DELETE CASCADE,
    previous_hash  TEXT NOT NULL,
    new_hash       TEXT NOT NULL,
    unified_diff   TEXT NOT NULL,                         -- capped at 64 KB
    changed_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS sync_runs (
    id              INTEGER PRIMARY KEY,
    source_id       INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    status          TEXT NOT NULL CHECK (status IN ('running', 'ok', 'error')),
    pages_added     INTEGER NOT NULL DEFAULT 0,
    pages_updated   INTEGER NOT NULL DEFAULT 0,
    pages_unchanged INTEGER NOT NULL DEFAULT 0,
    pages_stale     INTEGER NOT NULL DEFAULT 0,
    pages_failed    INTEGER NOT NULL DEFAULT 0,
    error_message   TEXT
);

-- External-content FTS5 tables (porter + unicode61 tokenization)
CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
    title, content_md,
    content='pages', content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS sections_fts USING fts5(
    heading_path, content_md,
    content='sections', content_rowid='id',
    tokenize='porter unicode61'
);

CREATE INDEX IF NOT EXISTS idx_pages_source_status ON pages(source_id, status);
CREATE INDEX IF NOT EXISTS idx_sections_page ON sections(page_id, ordinal);
CREATE INDEX IF NOT EXISTS idx_sections_source ON sections(source_id);
CREATE INDEX IF NOT EXISTS idx_sync_runs_source ON sync_runs(source_id, started_at DESC);

-- External-content FTS5 synchronization triggers for pages
CREATE TRIGGER IF NOT EXISTS pages_ai AFTER INSERT ON pages BEGIN
    INSERT INTO pages_fts(rowid, title, content_md)
    VALUES (new.id, new.title, new.content_md);
END;

CREATE TRIGGER IF NOT EXISTS pages_ad AFTER DELETE ON pages BEGIN
    INSERT INTO pages_fts(pages_fts, rowid, title, content_md)
    VALUES ('delete', old.id, old.title, old.content_md);
END;

CREATE TRIGGER IF NOT EXISTS pages_au AFTER UPDATE ON pages
WHEN old.id IS NOT new.id OR old.title IS NOT new.title OR old.content_md IS NOT new.content_md BEGIN
    INSERT INTO pages_fts(pages_fts, rowid, title, content_md)
    VALUES ('delete', old.id, old.title, old.content_md);
    INSERT INTO pages_fts(rowid, title, content_md)
    VALUES (new.id, new.title, new.content_md);
END;

-- External-content FTS5 synchronization triggers for sections
CREATE TRIGGER IF NOT EXISTS sections_ai AFTER INSERT ON sections BEGIN
    INSERT INTO sections_fts(rowid, heading_path, content_md)
    VALUES (new.id, new.heading_path, new.content_md);
END;

CREATE TRIGGER IF NOT EXISTS sections_ad AFTER DELETE ON sections BEGIN
    INSERT INTO sections_fts(sections_fts, rowid, heading_path, content_md)
    VALUES ('delete', old.id, old.heading_path, old.content_md);
END;

CREATE TRIGGER IF NOT EXISTS sections_au AFTER UPDATE ON sections
WHEN old.id IS NOT new.id OR old.heading_path IS NOT new.heading_path OR old.content_md IS NOT new.content_md BEGIN
    INSERT INTO sections_fts(sections_fts, rowid, heading_path, content_md)
    VALUES ('delete', old.id, old.heading_path, old.content_md);
    INSERT INTO sections_fts(rowid, heading_path, content_md)
    VALUES (new.id, new.heading_path, new.content_md);
END;

-- Convenience views for concise LLM and UI queries
CREATE VIEW IF NOT EXISTS v_sources AS
SELECT
    s.id,
    s.name,
    s.kind,
    s.origin,
    s.tags,
    s.include_patterns,
    s.exclude_patterns,
    s.max_pages,
    s.max_depth,
    s.render_js,
    s.refresh_interval_secs,
    s.status,
    s.last_synced_at,
    s.last_error,
    s.created_at,
    COALESCE(SUM(CASE WHEN p.status = 'active' THEN 1 ELSE 0 END), 0) AS active_pages,
    COALESCE(SUM(CASE WHEN p.status = 'stale' THEN 1 ELSE 0 END), 0) AS stale_pages,
    COALESCE(SUM(CASE WHEN p.status = 'error' THEN 1 ELSE 0 END), 0) AS error_pages,
    COALESCE(SUM(p.word_count), 0) AS total_words
FROM sources s
LEFT JOIN pages p ON p.source_id = s.id
GROUP BY s.id;

CREATE VIEW IF NOT EXISTS v_recent_changes AS
SELECT
    pr.page_id,
    p.source_id,
    s.name AS source_name,
    p.uri,
    p.title,
    pr.previous_hash,
    pr.new_hash,
    pr.unified_diff,
    pr.changed_at
FROM page_revisions pr
JOIN pages p ON p.id = pr.page_id
JOIN sources s ON s.id = p.source_id
ORDER BY pr.changed_at DESC, pr.page_id DESC;
