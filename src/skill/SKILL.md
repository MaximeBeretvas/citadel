---
name: web-vault
description: Query and manage local documentation and Markdown books/notes in web-vault via its query and manage MCP tools. Use when grounding answers in external library or framework docs, searching indexed local Markdown folders, checking available vault sources or recent doc changes and staleness, or adding, syncing, updating, and removing documentation sources.
---

# web-vault Grounding Skill

Prefer verified content from the local `web-vault` MCP server (`query` and `manage` tools) over memory whenever external docs or indexed local Markdown books/notes are relevant.

## SQLite Schema Summary

All `query` calls execute a single read-only `SELECT` statement (5s timeout, default `limit: 50`, max `200`, 100 KB payload cap):

- `v_sources(id, name, kind, origin, tags, status, active_pages, stale_pages, error_pages, total_words, last_synced_at)` — source inventory and health counts (`kind` is `'web'` or `'local'`).
- `pages(id, source_id, uri, title, content_md, content_hash, word_count, status, error_message, last_checked_at, last_changed_at)` — stored pages (`status`: `'active' | 'stale' | 'error'`).
- `sections(id, page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)` — H1–H3 split sections (`heading_path` e.g. `'Ingestion > CDC'`).
- `pages_fts(title, content_md)` — FTS5 index (`rowid = pages.id`, `porter unicode61`).
- `sections_fts(heading_path, content_md)` — primary FTS5 index (`rowid = sections.id`, `porter unicode61`).
- `page_revisions(page_id, previous_hash, new_hash, unified_diff, changed_at)` — latest normalized diff per page.
- `v_recent_changes(page_id, source_id, source_name, uri, title, previous_hash, new_hash, unified_diff, changed_at)` — recent changes joined with source and page metadata.

## 2-Step FTS5 Retrieval Recipe

### Step 0: Check Available Sources
```sql
SELECT name, kind, origin, tags, active_pages, stale_pages, error_pages, last_synced_at
FROM v_sources
ORDER BY name
```

### Step 1: Search Ranked Section Snippets
Filter by `src.name` when you know the target source slug, and keep `LIMIT` small:
```sql
SELECT
  s.id,
  src.name AS source,
  p.uri,
  s.heading_path,
  p.status,
  p.last_changed_at,
  snippet(sections_fts, 1, '[', ']', '...', 16) AS excerpt,
  bm25(sections_fts) AS rank
FROM sections_fts
JOIN sections s ON s.id = sections_fts.rowid
JOIN pages p ON p.id = s.page_id
JOIN sources src ON src.id = s.source_id
WHERE sections_fts MATCH 'term1 AND term2'
ORDER BY rank
LIMIT 15
```

### Step 2: Fetch Exact Section Bodies
Retrieve full `sections.content_md` only for relevant `s.id` values (or `substr(s.content_md, ...)` if a section is large):
```sql
SELECT
  s.id,
  src.name AS source,
  p.uri,
  s.heading_path,
  p.status,
  p.last_changed_at,
  s.content_md
FROM sections s
JOIN pages p ON p.id = s.page_id
JOIN sources src ON src.id = s.source_id
WHERE s.id IN (12, 34)
ORDER BY s.page_id, s.ordinal
```

## Changes & Staleness Checks

- **Recent diffs:**
  ```sql
  SELECT source_name, uri, title, changed_at, unified_diff
  FROM v_recent_changes
  LIMIT 20
  ```
- **Stale or error pages:**
  ```sql
  SELECT src.name AS source, p.uri, p.status, p.last_checked_at, p.error_message
  FROM pages p
  JOIN sources src ON src.id = p.source_id
  WHERE p.status != 'active'
  ORDER BY p.last_checked_at DESC
  ```

## Citation Rules

- Always cite the `source` slug, `uri`, and `heading_path` for retrieved facts.
- Note `last_changed_at` when recency or version drift matters.
- Explicitly call out if a retrieved page has `status = 'stale'` or `'error'` (last good content is retained rather than silently deleted).
- If a source is missing or outdated, use `manage` (`add` or `sync`) or report the gap rather than guessing.

## Managing Sources (`manage` Tool)

- **`add`**: Register and optionally ingest (`sync_now`, default `true`) an `http(s)://` URL (SSRF-checked) or existing local Markdown directory (`action`, required `origin`, optional `name`, `tags`, `refresh_interval_secs`, `max_pages`, `max_depth`, `include_patterns`, `exclude_patterns`, `render_js`, `sync_now`).
- **`sync`**: Refresh one source (`name`) or all sources when `name` is omitted (`action: "sync"`, optional `name`).
- **`update`**: Update configuration on an existing source (`action: "update"`, required `name`, optional `tags`, `refresh_interval_secs`, `max_pages`, `max_depth`, `include_patterns`, `exclude_patterns`, `render_js`).
- **`remove`**: Delete a source and cascade-remove its pages and sections (`action: "remove"`, required `name`).
