use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::config::VaultConfig;
use crate::mcp::{handle_jsonrpc_payload, jsonrpc_error};

/// Build the Axum router mounting MCP Streamable HTTP (`POST/GET /mcp`).
pub fn mcp_router(config: VaultConfig) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp))
        .with_state(config)
}

async fn post_mcp(State(config): State<VaultConfig>, body: Bytes) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(val) => val,
        Err(err) => {
            tracing::warn!(error = %err, "invalid JSON received on POST /mcp");
            let err_frame = jsonrpc_error(Value::Null, -32700, &format!("Parse error: {err}"));
            return (StatusCode::OK, Json(err_frame)).into_response();
        }
    };

    match handle_jsonrpc_payload(&config, payload).await {
        Some(response) => (StatusCode::OK, Json(response)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn get_mcp(headers: HeaderMap) -> Response {
    let wants_sse = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|val| val.to_ascii_lowercase().contains("text/event-stream"));

    if wants_sse {
        return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "name": "web-vault",
            "version": env!("CARGO_PKG_VERSION"),
            "transport": "streamable-http",
            "endpoint": "/mcp"
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;

    async fn read_json(resp: axum::response::Response) -> Value {
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "invalid JSON body ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }

    #[tokio::test]
    async fn post_mcp_initialize_and_tools_list() {
        let vault_dir = tempdir().expect("vault tempdir");
        let cfg = VaultConfig::new(vault_dir.path());
        let app = mcp_router(cfg);

        // 1. initialize
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "http-test", "version": "0.1" }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(init_req.to_string()))
                    .expect("build initialize request"),
            )
            .await
            .expect("initialize response");
        assert_eq!(resp.status(), StatusCode::OK);
        let init_json = read_json(resp).await;
        assert_eq!(init_json["jsonrpc"], "2.0");
        assert_eq!(init_json["id"], 1);
        assert_eq!(init_json["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init_json["result"]["serverInfo"]["name"], "web-vault");

        // 2. notification -> 202 Accepted with empty body
        let notif_req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(notif_req.to_string()))
                    .expect("build notification request"),
            )
            .await
            .expect("notification response");
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let notif_bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect notification body")
            .to_bytes();
        assert!(notif_bytes.is_empty());

        // 3. tools/list -> exactly ["query", "manage"]
        let list_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(list_req.to_string()))
                    .expect("build tools/list request"),
            )
            .await
            .expect("tools/list response");
        assert_eq!(resp.status(), StatusCode::OK);
        let list_json = read_json(resp).await;
        let tools = list_json["result"]["tools"]
            .as_array()
            .expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
            .collect();
        assert_eq!(names, vec!["query", "manage"]);

        // 4. GET /mcp (JSON info vs text/event-stream -> 405)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/mcp")
                    .body(Body::empty())
                    .expect("build GET /mcp request"),
            )
            .await
            .expect("GET /mcp response");
        assert_eq!(resp.status(), StatusCode::OK);
        let info = read_json(resp).await;
        assert_eq!(info["name"], "web-vault");
        assert_eq!(info["transport"], "streamable-http");
        assert_eq!(info["endpoint"], "/mcp");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/mcp")
                    .header(header::ACCEPT, "text/event-stream")
                    .body(Body::empty())
                    .expect("build GET /mcp SSE request"),
            )
            .await
            .expect("GET /mcp SSE response");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            resp.headers()
                .get(header::ALLOW)
                .and_then(|v| v.to_str().ok()),
            Some("POST")
        );

        // 5. Invalid JSON -> JSON-RPC parse error (-32700), empty batch -> -32600
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{not valid json"))
                    .expect("build invalid JSON request"),
            )
            .await
            .expect("invalid JSON response");
        assert_eq!(resp.status(), StatusCode::OK);
        let err_json = read_json(resp).await;
        assert_eq!(err_json["error"]["code"], -32700);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("[]"))
                    .expect("build empty batch request"),
            )
            .await
            .expect("empty batch response");
        assert_eq!(resp.status(), StatusCode::OK);
        let batch_err = read_json(resp).await;
        assert_eq!(batch_err["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn post_mcp_query_and_manage_roundtrip() {
        let vault_dir = tempdir().expect("vault tempdir");
        let docs_dir = tempdir().expect("docs tempdir");
        std::fs::write(
            docs_dir.path().join("guide.md"),
            "# HTTP MCP\n\n## Transport\n\nStreamable HTTP shares the MCP dispatcher.\n",
        )
        .expect("write guide.md");

        let cfg = VaultConfig::new(vault_dir.path());
        let app = mcp_router(cfg);

        // 0. Query on a fresh empty vault initializes DB and succeeds
        let empty_query = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "query",
                "arguments": { "sql": "SELECT name FROM v_sources" }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(empty_query.to_string()))
                    .expect("build empty query request"),
            )
            .await
            .expect("empty query response");
        assert_eq!(resp.status(), StatusCode::OK);
        let empty_json = read_json(resp).await;
        assert_eq!(empty_json["result"]["isError"], false);

        // 1. manage(add) local folder
        let add_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "manage",
                "arguments": {
                    "action": "add",
                    "origin": docs_dir.path().to_string_lossy(),
                    "name": "http-guide"
                }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(add_req.to_string()))
                    .expect("build manage add request"),
            )
            .await
            .expect("manage add response");
        assert_eq!(resp.status(), StatusCode::OK);
        let add_json = read_json(resp).await;
        assert_eq!(add_json["result"]["isError"], false);
        let add_payload: Value =
            serde_json::from_str(add_json["result"]["content"][0]["text"].as_str().unwrap())
                .expect("add payload JSON");
        assert_eq!(add_payload["source"]["name"], "http-guide");
        assert_eq!(add_payload["sync_summary"]["pages_added"], 1);

        // 2. query(SELECT ...) via FTS5
        let query_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "query",
                "arguments": {
                    "sql": "SELECT src.name, p.uri, s.heading_path FROM sections_fts JOIN sections s ON s.id = sections_fts.rowid JOIN pages p ON p.id = s.page_id JOIN sources src ON src.id = s.source_id WHERE sections_fts MATCH 'dispatcher'"
                }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(query_req.to_string()))
                    .expect("build query request"),
            )
            .await
            .expect("query response");
        assert_eq!(resp.status(), StatusCode::OK);
        let query_json = read_json(resp).await;
        assert_eq!(query_json["result"]["isError"], false);
        let query_payload: Value =
            serde_json::from_str(query_json["result"]["content"][0]["text"].as_str().unwrap())
                .expect("query payload JSON");
        assert_eq!(query_payload["row_count"], 1);
        assert_eq!(query_payload["rows"][0][0], "http-guide");
        assert_eq!(query_payload["rows"][0][1], "guide.md");
        assert_eq!(query_payload["rows"][0][2], "HTTP MCP > Transport");

        // 3. Read-only guard enforced on query
        let write_req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "query",
                "arguments": { "sql": "DELETE FROM pages" }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(write_req.to_string()))
                    .expect("build write query request"),
            )
            .await
            .expect("write query response");
        assert_eq!(resp.status(), StatusCode::OK);
        let write_json = read_json(resp).await;
        assert_eq!(write_json["result"]["isError"], true);

        // 4. SSRF guard enforced on manage(add)
        let ssrf_req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "manage",
                "arguments": {
                    "action": "add",
                    "origin": "http://127.0.0.1/admin"
                }
            }
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(ssrf_req.to_string()))
                    .expect("build ssrf request"),
            )
            .await
            .expect("ssrf response");
        assert_eq!(resp.status(), StatusCode::OK);
        let ssrf_json = read_json(resp).await;
        assert_eq!(ssrf_json["result"]["isError"], true);
    }
}
