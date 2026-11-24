use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::VaultConfig;
use crate::daemon;
use crate::db::open_rw_db;
use crate::error::{Result, VaultError};
use crate::ui::build_serve_router;

/// Default bind address for `citadel serve`.
pub const DEFAULT_SERVE_BIND: &str = "0.0.0.0:4317";

/// Bind a TCP listener on `bind_addr` and return both the listener and its resolved local address.
pub async fn bind_listener(bind_addr: &str) -> Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
}

/// Run the unified `serve` process (scheduler + watcher + Web UI + REST API + MCP HTTP)
/// using a pre-bound [`TcpListener`] until `cancel` is triggered.
pub async fn run_serve_with_listener(
    config: VaultConfig,
    listener: TcpListener,
    cancel: CancellationToken,
) -> Result<()> {
    // Ensure vault.db and schema exist on fresh vaults before serving requests or starting tasks.
    let _conn = open_rw_db(&config.db_path())?;

    let scheduler_cfg = config.clone();
    let scheduler_cancel = cancel.clone();
    let scheduler_handle =
        tokio::spawn(async move { daemon::run_scheduler(scheduler_cfg, scheduler_cancel).await });

    let watcher_cfg = config.clone();
    let watcher_cancel = cancel.clone();
    let watcher_handle =
        tokio::spawn(async move { daemon::run_watcher(watcher_cfg, watcher_cancel).await });

    let app = build_serve_router(config);
    let server_res = axum::serve(listener, app)
        .with_graceful_shutdown(cancel.clone().cancelled_owned())
        .await
        .map_err(|err| VaultError::Other(format!("serve HTTP error: {err}")));

    // Ensure background tasks exit if the HTTP server returned early.
    cancel.cancel();

    let sched_res = scheduler_handle
        .await
        .map_err(|err| VaultError::Other(format!("scheduler task join error: {err}")))?;
    let watch_res = watcher_handle
        .await
        .map_err(|err| VaultError::Other(format!("watcher task join error: {err}")))?;

    server_res?;
    sched_res?;
    watch_res?;
    Ok(())
}

/// Bind `bind_addr` and run the unified `serve` process until `cancel` is triggered.
pub async fn run_serve(
    config: VaultConfig,
    bind_addr: &str,
    cancel: CancellationToken,
) -> Result<()> {
    let (listener, actual_addr) = bind_listener(bind_addr).await?;
    tracing::info!(
        bind = %actual_addr,
        ui = %format!("http://{actual_addr}/"),
        mcp = %format!("http://{actual_addr}/mcp"),
        "citadel serve listening"
    );
    run_serve_with_listener(config, listener, cancel).await
}

/// Bind `bind_addr` and run the unified `serve` process until `SIGINT` or `SIGTERM`.
pub async fn run_serve_until_signal(config: VaultConfig, bind_addr: &str) -> Result<()> {
    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();

    let signal_task = tokio::spawn(async move {
        daemon::wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received; stopping citadel serve");
        signal_cancel.cancel();
    });

    let res = run_serve(config, bind_addr, cancel.clone()).await;
    cancel.cancel();
    signal_task.abort();
    res
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{json, Value};
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn serves_ui_api_health_and_mcp_on_single_port() {
        let vault_dir = tempdir().expect("vault tempdir");
        let config = VaultConfig::new(vault_dir.path());
        let (listener, addr) = bind_listener("127.0.0.1:0")
            .await
            .expect("bind ephemeral loopback listener");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_task =
            tokio::spawn(
                async move { run_serve_with_listener(config, listener, server_cancel).await },
            );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client");
        let base = format!("http://{addr}");

        // 1. Embedded Web UI at /
        let ui_resp = client
            .get(format!("{base}/"))
            .send()
            .await
            .expect("GET / response");
        assert_eq!(ui_resp.status(), reqwest::StatusCode::OK);
        let ui_ct = ui_resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ui_ct.starts_with("text/html"), "unexpected CT: {ui_ct}");
        let ui_html = ui_resp.text().await.expect("UI HTML body");
        assert!(ui_html.to_ascii_lowercase().contains("<html"));

        // 2. REST API health at /api/health
        let health_resp = client
            .get(format!("{base}/api/health"))
            .send()
            .await
            .expect("GET /api/health response");
        assert_eq!(health_resp.status(), reqwest::StatusCode::OK);
        let health_body = health_resp.text().await.expect("health text");
        let health_json: Value = serde_json::from_str(&health_body).expect("health JSON");
        assert_eq!(health_json["status"], "ok");

        // 3. MCP Streamable HTTP at POST /mcp
        let mcp_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        })
        .to_string();
        let mcp_resp = client
            .post(format!("{base}/mcp"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(mcp_body)
            .send()
            .await
            .expect("POST /mcp response");
        assert_eq!(mcp_resp.status(), reqwest::StatusCode::OK);
        let mcp_text = mcp_resp.text().await.expect("MCP text");
        let mcp_json: Value = serde_json::from_str(&mcp_text).expect("MCP JSON");
        let tools = mcp_json["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
            .collect();
        assert_eq!(names, vec!["query", "manage"]);

        // 4. Graceful shutdown via CancellationToken
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve should shut down within 5s")
            .expect("serve task should not panic");
        assert!(res.is_ok(), "serve returned error on shutdown: {res:?}");
    }
}
