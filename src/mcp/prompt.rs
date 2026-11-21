use serde_json::{json, Value};

const QUERY_DESCRIPTION: &str = "\
Execute a single read-only SQLite SELECT statement against the knowledge vault (5s timeout, max 200 rows, 100 KB cap).

Schema:
- v_sources(id, name, kind, origin, tags, status, active_pages, stale_pages, error_pages, total_words, last_synced_at)
- pages(id, source_id, uri, title, content_md, content_hash, word_count, status, error_message, last_checked_at, last_changed_at)
- sections(id, page_id, source_id, ordinal, heading, heading_path, level, content_md, word_count)
- pages_fts(title, content_md) [rowid = pages.id]
- sections_fts(heading_path, content_md) [rowid = sections.id]
- page_revisions(page_id, previous_hash, new_hash, unified_diff, changed_at)
- v_recent_changes(page_id, source_id, source_name, uri, title, previous_hash, new_hash, unified_diff, changed_at)

2-step retrieval recipe:
1. Search v_sources or sections_fts for snippets/IDs:
   SELECT s.id, src.name AS source, p.uri, s.heading_path, snippet(sections_fts, 1, '[', ']', '...', 16) AS excerpt, bm25(sections_fts) AS rank FROM sections_fts JOIN sections s ON s.id = sections_fts.rowid JOIN pages p ON p.id = s.page_id JOIN sources src ON src.id = s.source_id WHERE sections_fts MATCH 'term' ORDER BY rank LIMIT 15
2. Fetch exact sections.content_md by id:
   SELECT s.heading_path, s.content_md, p.uri, p.last_changed_at FROM sections s JOIN pages p ON p.id = s.page_id WHERE s.id IN (...)";

const MANAGE_DESCRIPTION: &str = "\
Manage knowledge vault sources: add, sync, update, or remove documentation URLs (http/https, SSRF-checked) and local Markdown folders.";

/// Return the MCP `tools/list` result object exposing strictly `["query", "manage"]`.
pub fn tools_list_json() -> Value {
    json!({
        "tools": [
            {
                "name": "query",
                "description": QUERY_DESCRIPTION,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sql": {
                            "type": "string",
                            "description": "Single read-only SQLite SELECT statement against sources, v_sources, pages, sections, pages_fts, sections_fts, page_revisions, or v_recent_changes."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 200,
                            "default": 50,
                            "description": "Maximum rows to return (default 50, max 200)."
                        }
                    },
                    "required": ["sql"]
                }
            },
            {
                "name": "manage",
                "description": MANAGE_DESCRIPTION,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["add", "sync", "update", "remove"],
                            "description": "Vault management operation to perform."
                        },
                        "origin": {
                            "type": "string",
                            "description": "Required for 'add': http(s):// root URL or absolute local Markdown folder path."
                        },
                        "name": {
                            "type": "string",
                            "description": "Source slug. Optional for 'add' (auto-derived if omitted); required for 'update'/'remove'; optional for 'sync' (syncs all if omitted)."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "refresh_interval_secs": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "0 = manual only."
                        },
                        "max_pages": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 5000
                        },
                        "max_depth": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 20
                        },
                        "include_patterns": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "exclude_patterns": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "render_js": {
                            "type": "boolean"
                        },
                        "sync_now": {
                            "type": "boolean",
                            "default": true,
                            "description": "For 'add': whether to run initial ingestion immediately (default true)."
                        }
                    },
                    "required": ["action"]
                }
            }
        ]
    })
}
