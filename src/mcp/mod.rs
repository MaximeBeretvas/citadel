pub mod http;
pub mod prompt;

use std::io::{BufRead, Write};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::VaultConfig;
use crate::db::{execute_readonly_query, open_rw_db};
use crate::error::{Result, VaultError};
use crate::sources::{AddSourceParams, Removal, SourceLifecycle, SyncOutcome, UpdateSourceParams};

const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05"];

#[derive(Debug, Deserialize)]
struct QueryArgs {
    sql: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ManageArgs {
    action: String,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    refresh_interval_secs: Option<i64>,
    #[serde(default)]
    max_pages: Option<u32>,
    #[serde(default)]
    max_depth: Option<u32>,
    #[serde(default)]
    include_patterns: Option<Vec<String>>,
    #[serde(default)]
    exclude_patterns: Option<Vec<String>>,
    #[serde(default)]
    render_js: Option<bool>,
    #[serde(default)]
    sync_now: Option<bool>,
}

/// Dispatch a single parsed JSON-RPC 2.0 request or notification value.
///
/// Returns `None` for notifications (missing `"id"` or `notifications/*` methods),
/// so both `stdio` and Streamable HTTP transports can share identical dispatch logic.
pub async fn handle_jsonrpc_value(config: &VaultConfig, request: Value) -> Option<Value> {
    let Some(obj) = request.as_object() else {
        return Some(jsonrpc_error(
            Value::Null,
            -32600,
            "Invalid Request: expected JSON object",
        ));
    };

    let method = obj.get("method").and_then(Value::as_str);
    let id = obj.get("id").cloned();

    // JSON-RPC 2.0 notifications omit `id`; MCP notifications also begin with `notifications/`.
    if id.is_none() || method.is_some_and(|m| m.starts_with("notifications/")) {
        tracing::debug!(
            method = method.unwrap_or("<unknown>"),
            "handled MCP notification"
        );
        return None;
    }

    let req_id = id?;

    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(jsonrpc_error(
            req_id,
            -32600,
            "Invalid Request: 'jsonrpc' must be '2.0'",
        ));
    }

    let Some(method_name) = method else {
        return Some(jsonrpc_error(
            req_id,
            -32600,
            "Invalid Request: missing string 'method'",
        ));
    };

    let params = match obj.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(val @ Value::Object(_)) => val.clone(),
        Some(_) => {
            return Some(jsonrpc_error(
                req_id,
                -32602,
                "Invalid params: 'params' must be an object",
            ));
        }
    };

    tracing::debug!(method = method_name, "dispatching MCP request");

    let response = match method_name {
        "initialize" => handle_initialize(req_id, &params),
        "ping" => jsonrpc_result(req_id, json!({})),
        "tools/list" => jsonrpc_result(req_id, prompt::tools_list_json()),
        "tools/call" => handle_tools_call(config, req_id, &params).await,
        other => jsonrpc_error(req_id, -32601, &format!("Method not found: {other}")),
    };

    Some(response)
}

/// Dispatch a parsed JSON-RPC 2.0 payload (single object or batch array).
///
/// Returns `None` when the payload consists solely of notifications.
pub async fn handle_jsonrpc_payload(config: &VaultConfig, payload: Value) -> Option<Value> {
    match payload {
        Value::Array(items) => {
            if items.is_empty() {
                return Some(jsonrpc_error(
                    Value::Null,
                    -32600,
                    "Invalid Request: empty JSON-RPC batch",
                ));
            }
            let mut responses = Vec::with_capacity(items.len());
            for item in items {
                if let Some(resp) = handle_jsonrpc_value(config, item).await {
                    responses.push(resp);
                }
            }
            if responses.is_empty() {
                None
            } else {
                Some(Value::Array(responses))
            }
        }
        single => handle_jsonrpc_value(config, single).await,
    }
}

/// Run the newline-delimited JSON-RPC 2.0 `stdio` loop over arbitrary `BufRead` / `Write` streams.
pub fn run_stdio_with_io<R: BufRead, W: Write>(
    config: &VaultConfig,
    mut reader: R,
    mut writer: W,
) -> Result<()> {
    ensure_db_initialized(config)?;
    tracing::debug!(db_path = %config.db_path().display(), "MCP stdio server ready");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(payload) => {
                if let Some(resp) = rt.block_on(handle_jsonrpc_payload(config, payload)) {
                    write_json_frame(&mut writer, &resp)?;
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "invalid JSON received on MCP stdin");
                let err_frame = jsonrpc_error(Value::Null, -32700, &format!("Parse error: {err}"));
                write_json_frame(&mut writer, &err_frame)?;
            }
        }
    }

    tracing::debug!("MCP stdio stream closed");
    Ok(())
}

/// Run the `web-vault mcp` `stdio` server reading `stdin` and emitting JSON-RPC frames strictly to `stdout`.
pub fn run_stdio_server(config: &VaultConfig) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_stdio_with_io(config, stdin.lock(), stdout.lock())
}

fn ensure_db_initialized(config: &VaultConfig) -> Result<()> {
    let db_path = config.db_path();
    if !db_path.exists() {
        let conn = open_rw_db(&db_path)?;
        drop(conn);
    }
    Ok(())
}

fn handle_initialize(req_id: Value, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let negotiated = requested
        .and_then(|v| {
            SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .copied()
                .find(|s| *s == v)
        })
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    jsonrpc_result(
        req_id,
        json!({
            "protocolVersion": negotiated,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "web-vault",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

async fn handle_tools_call(config: &VaultConfig, req_id: Value, params: &Value) -> Value {
    let Some(tool_name) = params.get("name").and_then(Value::as_str) else {
        return jsonrpc_error(req_id, -32602, "Invalid params: missing tool 'name'");
    };

    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(val @ Value::Object(_)) => val.clone(),
        Some(_) => {
            return jsonrpc_error(
                req_id,
                -32602,
                "Invalid params: 'arguments' must be an object",
            );
        }
    };

    tracing::debug!(tool = tool_name, "invoking MCP tool");

    match tool_name {
        "query" => {
            let outcome = match call_query_tool(config, arguments).await {
                Ok(payload) => tool_ok_json(&payload),
                Err(msg) => tool_error(&msg),
            };
            jsonrpc_result(req_id, outcome)
        }
        "manage" => {
            let outcome = match call_manage_tool(config, arguments).await {
                Ok(payload) => tool_ok_json(&payload),
                Err(msg) => tool_error(&msg),
            };
            jsonrpc_result(req_id, outcome)
        }
        unknown => jsonrpc_error(
            req_id,
            -32602,
            &format!("Unknown tool '{unknown}'; available tools: query, manage"),
        ),
    }
}

async fn call_query_tool(
    config: &VaultConfig,
    arguments: Value,
) -> std::result::Result<Value, String> {
    let args: QueryArgs = serde_json::from_value(arguments)
        .map_err(|err| format!("invalid arguments for 'query': {err}"))?;

    let config = config.clone();
    let result = tokio::task::spawn_blocking(move || {
        ensure_db_initialized(&config)?;
        execute_readonly_query(&config.db_path(), &args.sql, args.limit)
    })
    .await
    .map_err(|err| format!("query task failed: {err}"))?
    .map_err(|err| err.to_string())?;

    serde_json::to_value(result).map_err(|err| format!("failed to serialize query result: {err}"))
}

async fn call_manage_tool(
    config: &VaultConfig,
    arguments: Value,
) -> std::result::Result<Value, String> {
    let args: ManageArgs = serde_json::from_value(arguments)
        .map_err(|err| format!("invalid arguments for 'manage': {err}"))?;

    match args.action.trim() {
        "add" => call_manage_add(config, args).await,
        "sync" => call_manage_sync(config, args).await,
        "update" => call_manage_update(config, args).await,
        "remove" => call_manage_remove(config, args).await,
        other => Err(format!(
            "unsupported manage action '{other}'; expected one of: add, sync, update, remove"
        )),
    }
}

async fn call_manage_add(
    config: &VaultConfig,
    args: ManageArgs,
) -> std::result::Result<Value, String> {
    let raw_origin = args
        .origin
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing required parameter 'origin' for action 'add'".to_string())?;

    let sync_now = args.sync_now.unwrap_or(true);
    let params = AddSourceParams {
        origin: raw_origin.to_string(),
        name: args.name,
        tags: args.tags.unwrap_or_default(),
        refresh_interval_secs: args.refresh_interval_secs,
        max_pages: args.max_pages,
        max_depth: args.max_depth,
        include_patterns: args.include_patterns.unwrap_or_default(),
        exclude_patterns: args.exclude_patterns.unwrap_or_default(),
        render_js: args.render_js.unwrap_or(false),
    };

    let added = SourceLifecycle::new(config.clone())
        .add(params, sync_now)
        .await
        .map_err(|err| err.to_string())?;

    Ok(json!({
        "action": "add",
        "source": added.source,
        "sync_summary": added.summary,
    }))
}

async fn call_manage_sync(
    config: &VaultConfig,
    args: ManageArgs,
) -> std::result::Result<Value, String> {
    let target_name = match args.name.as_deref() {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err("source 'name' must not be empty when provided".to_string());
            }
            Some(trimmed)
        }
        None => None,
    };

    let lifecycle = SourceLifecycle::new(config.clone());
    let Some(name) = target_name else {
        let summaries = lifecycle
            .sync_all(false)
            .await
            .map_err(|err| err.to_string())?;
        return Ok(json!({
            "action": "sync",
            "summaries": summaries,
        }));
    };

    match lifecycle
        .sync(name, false)
        .await
        .map_err(|err| err.to_string())?
    {
        SyncOutcome::Synced(summary) => Ok(json!({
            "action": "sync",
            "summaries": [summary],
        })),
        SyncOutcome::AlreadySyncing => Ok(json!({
            "action": "sync",
            "outcome": "already_syncing",
            "name": name,
            "summaries": [],
            "message": format!("source '{name}' is already syncing; nothing was started"),
        })),
    }
}

async fn call_manage_update(
    config: &VaultConfig,
    args: ManageArgs,
) -> std::result::Result<Value, String> {
    let name = require_non_empty_name(args.name.as_deref(), "update")?;
    let params = UpdateSourceParams {
        tags: args.tags,
        refresh_interval_secs: args.refresh_interval_secs,
        max_pages: args.max_pages,
        max_depth: args.max_depth,
        include_patterns: args.include_patterns,
        exclude_patterns: args.exclude_patterns,
        render_js: args.render_js,
    };

    let lifecycle = SourceLifecycle::new(config.clone());
    let name = name.to_string();
    let source = tokio::task::spawn_blocking(move || lifecycle.update(&name, params))
        .await
        .map_err(|err| format!("update task failed: {err}"))?
        .map_err(|err| err.to_string())?;

    Ok(json!({
        "action": "update",
        "source": source,
    }))
}

async fn call_manage_remove(
    config: &VaultConfig,
    args: ManageArgs,
) -> std::result::Result<Value, String> {
    let name = require_non_empty_name(args.name.as_deref(), "remove")?;
    let removal = SourceLifecycle::new(config.clone())
        .remove(name)
        .await
        .map_err(|err| err.to_string())?;

    Ok(match removal {
        Removal::Removed(removed) => json!({
            "action": "remove",
            "name": removed.name,
            "pages_deleted": removed.pages_deleted,
        }),
        Removal::AlreadySyncing => json!({
            "action": "remove",
            "outcome": "already_syncing",
            "name": name,
            "message": format!("source '{name}' is syncing; nothing was removed, try again when it finishes"),
        }),
    })
}

fn require_non_empty_name<'a>(
    raw: Option<&'a str>,
    action: &str,
) -> std::result::Result<&'a str, String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing required parameter 'name' for action '{action}'"))
}

fn tool_ok_json(payload: &Value) -> Value {
    let text = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "isError": false
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": message
            }
        ],
        "isError": true
    })
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub(crate) fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn write_json_frame<W: Write>(writer: &mut W, frame: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, frame)
        .map_err(|err| VaultError::Other(format!("failed to serialize JSON-RPC frame: {err}")))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[tokio::test]
    async fn initialize_and_tools_list_exposes_exactly_two_tools() {
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path());

        let init_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0.1" }
                }
            }),
        )
        .await
        .expect("initialize should emit response");
        assert_eq!(init_resp["jsonrpc"], "2.0");
        assert_eq!(init_resp["id"], 1);
        assert_eq!(init_resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(init_resp["result"]["serverInfo"]["name"], "web-vault");
        assert!(init_resp["result"]["capabilities"]["tools"].is_object());

        let notif_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )
        .await;
        assert!(notif_resp.is_none(), "notification must not emit response");

        let list_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
        )
        .await
        .expect("tools/list should emit response");

        let tools = list_resp["result"]["tools"]
            .as_array()
            .expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
            .collect();
        assert_eq!(names, vec!["query", "manage"]);

        let query_desc = tools[0]["description"].as_str().expect("query description");
        for token in [
            "v_sources",
            "pages",
            "sections",
            "pages_fts",
            "sections_fts",
            "page_revisions",
            "v_recent_changes",
            "MATCH",
            "bm25",
            "snippet",
        ] {
            assert!(
                query_desc.contains(token),
                "query description missing '{token}'"
            );
        }

        assert_eq!(tools[0]["inputSchema"]["required"], json!(["sql"]));
        assert!(tools[0]["inputSchema"]["properties"]["sql"].is_object());
        assert!(tools[0]["inputSchema"]["properties"]["limit"].is_object());

        let manage_props = &tools[1]["inputSchema"]["properties"];
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["action"]));
        assert_eq!(
            manage_props["action"]["enum"],
            json!(["add", "sync", "update", "remove"])
        );
        for field in [
            "origin",
            "name",
            "tags",
            "refresh_interval_secs",
            "max_pages",
            "max_depth",
            "include_patterns",
            "exclude_patterns",
            "render_js",
            "sync_now",
        ] {
            assert!(
                manage_props[field].is_object(),
                "manage schema missing property `{field}`"
            );
        }
    }

    #[tokio::test]
    async fn query_tool_executes_fts5_and_blocks_writes() {
        let vault_dir = tempdir().expect("vault tempdir");
        let docs_dir = tempdir().expect("docs tempdir");
        std::fs::write(
            docs_dir.path().join("wal.md"),
            "# SQLite WAL\n\n## Concurrency\n\nWAL enables concurrent readers and writers.\n",
        )
        .expect("write wal.md");

        let cfg = VaultConfig::new(vault_dir.path());
        crate::sources::SourceLifecycle::new(cfg.clone())
            .add(
                crate::sources::AddSourceParams {
                    origin: docs_dir.path().to_string_lossy().into_owned(),
                    name: Some("sqlite-docs".to_string()),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("seed local source");

        let ok_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "query",
                    "arguments": {
                        "sql": "SELECT src.name, p.uri, s.heading_path FROM sections_fts JOIN sections s ON s.id = sections_fts.rowid JOIN pages p ON p.id = s.page_id JOIN sources src ON src.id = s.source_id WHERE sections_fts MATCH 'concurrent'"
                    }
                }
            }),
        )
        .await
        .expect("query response");

        let result = &ok_resp["result"];
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().expect("text content");
        let payload: Value = serde_json::from_str(text).expect("QueryResult JSON");
        assert_eq!(payload["row_count"], 1);
        assert_eq!(payload["rows"][0][0], "sqlite-docs");
        assert_eq!(payload["rows"][0][1], "wal.md");
        assert_eq!(payload["rows"][0][2], "SQLite WAL > Concurrency");

        for bad_sql in ["DELETE FROM pages", "PRAGMA query_only = OFF", "BAD SQL"] {
            let err_resp = handle_jsonrpc_value(
                &cfg,
                json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {
                        "name": "query",
                        "arguments": { "sql": bad_sql }
                    }
                }),
            )
            .await
            .expect("error response");
            assert_eq!(
                err_resp["result"]["isError"], true,
                "expected isError=true for `{bad_sql}`"
            );
        }
    }

    #[tokio::test]
    async fn manage_tool_add_sync_update_remove_local_source() {
        let vault_dir = tempdir().expect("vault tempdir");
        let docs_dir = tempdir().expect("docs tempdir");
        std::fs::write(
            docs_dir.path().join("intro.md"),
            "# Intro\n\nFirst version text.\n",
        )
        .expect("write intro.md");

        let cfg = VaultConfig::new(vault_dir.path());
        let docs_path = docs_dir.path().to_string_lossy().into_owned();

        // 1. Add local source
        let add_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": {
                        "action": "add",
                        "origin": docs_path,
                        "name": "local-guide",
                        "tags": ["guide"]
                    }
                }
            }),
        )
        .await
        .expect("add response");
        assert_eq!(add_resp["result"]["isError"], false);
        let add_payload: Value =
            serde_json::from_str(add_resp["result"]["content"][0]["text"].as_str().unwrap())
                .expect("add payload JSON");
        assert_eq!(add_payload["source"]["name"], "local-guide");
        assert_eq!(add_payload["sync_summary"]["pages_added"], 1);

        // 2. Query added content
        let query_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "query",
                    "arguments": {
                        "sql": "SELECT name, active_pages FROM v_sources WHERE name = 'local-guide'"
                    }
                }
            }),
        )
        .await
        .expect("query response");
        assert_eq!(query_resp["result"]["isError"], false);
        let query_payload: Value =
            serde_json::from_str(query_resp["result"]["content"][0]["text"].as_str().unwrap())
                .expect("query payload JSON");
        assert_eq!(query_payload["rows"], json!([["local-guide", 1]]));

        // 3. Modify file & sync
        std::fs::write(
            docs_dir.path().join("intro.md"),
            "# Intro\n\nSecond version text.\n",
        )
        .expect("update intro.md");
        let sync_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": { "action": "sync", "name": "local-guide" }
                }
            }),
        )
        .await
        .expect("sync response");
        assert_eq!(sync_resp["result"]["isError"], false);
        let sync_payload: Value =
            serde_json::from_str(sync_resp["result"]["content"][0]["text"].as_str().unwrap())
                .expect("sync payload JSON");
        assert_eq!(sync_payload["summaries"][0]["pages_updated"], 1);

        // 4. Update tags
        let update_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": {
                        "action": "update",
                        "name": "local-guide",
                        "tags": ["guide", "updated"]
                    }
                }
            }),
        )
        .await
        .expect("update response");
        assert_eq!(update_resp["result"]["isError"], false);
        let update_payload: Value = serde_json::from_str(
            update_resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .expect("update payload JSON");
        assert_eq!(
            update_payload["source"]["tags"],
            json!(["guide", "updated"])
        );

        // 5. SSRF URL rejected by manage
        let ssrf_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": {
                        "action": "add",
                        "origin": "http://169.254.169.254/latest/meta-data/"
                    }
                }
            }),
        )
        .await
        .expect("ssrf response");
        assert_eq!(ssrf_resp["result"]["isError"], true);

        // 6. Public web URL with sync_now=false passes SSRF check and registers cleanly
        let web_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": {
                        "action": "add",
                        "origin": "https://docs.example.com",
                        "sync_now": false
                    }
                }
            }),
        )
        .await
        .expect("web add response");
        assert_eq!(web_resp["result"]["isError"], false);
        let web_payload: Value =
            serde_json::from_str(web_resp["result"]["content"][0]["text"].as_str().unwrap())
                .expect("web add payload JSON");
        assert_eq!(web_payload["source"]["kind"], "web");
        assert_eq!(web_payload["source"]["name"], "docs-example-com");
        assert!(web_payload["sync_summary"].is_null());

        // 7. Remove source
        let rm_resp = handle_jsonrpc_value(
            &cfg,
            json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "manage",
                    "arguments": {
                        "action": "remove",
                        "name": "local-guide"
                    }
                }
            }),
        )
        .await
        .expect("remove response");
        assert_eq!(rm_resp["result"]["isError"], false);
        let rm_payload: Value =
            serde_json::from_str(rm_resp["result"]["content"][0]["text"].as_str().unwrap())
                .expect("remove payload JSON");
        assert_eq!(rm_payload["name"], "local-guide");
        assert_eq!(rm_payload["pages_deleted"], 1);
    }

    #[test]
    fn stdio_stdout_contains_only_valid_jsonrpc_frames() {
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path());

        let input = concat!(
            "\n",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"0.1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"query\",\"arguments\":{\"sql\":\"SELECT name FROM v_sources\"}}}\n"
        );

        let mut out = Vec::new();
        run_stdio_with_io(&cfg, Cursor::new(input.as_bytes()), &mut out)
            .expect("stdio loop should succeed");

        let text = String::from_utf8(out).expect("utf-8 stdout");
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3, "expected 3 JSON-RPC frames, got: {text:?}");

        let frames: Vec<Value> = lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("line must be valid JSON"))
            .collect();
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(frames[1]["id"], 2);
        assert_eq!(frames[2]["id"], 3);
        assert_eq!(frames[2]["result"]["isError"], false);
    }
}
