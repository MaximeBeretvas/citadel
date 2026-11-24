use std::net::SocketAddr;

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use tempfile::tempdir;
use tokio::net::TcpListener;

use super::*;
use crate::config::VaultConfig;
use crate::error::VaultError;
use crate::ingest::{SourceKind, SyncSummary};

fn synced(outcome: SyncOutcome) -> SyncSummary {
    match outcome {
        SyncOutcome::Synced(summary) => summary,
        SyncOutcome::AlreadySyncing => panic!("expected Synced, got AlreadySyncing"),
    }
}

/// A one-page site whose page takes `delay` to respond, so a Sync holds its claim that long.
async fn spawn_slow_site(delay: std::time::Duration) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/docs/",
        get(move || async move {
            tokio::time::sleep(delay).await;
            Html("<html><body><main><h1>Slow</h1><p>Eventually.</p></main></body></html>")
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, handle)
}

async fn spawn_two_page_site() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/docs/",
            get(|| async {
                Html(
                    r#"<html><body><nav><a href="/docs/next">Next</a></nav><main><h1>Docs</h1><p>Root page.</p></main></body></html>"#,
                )
            }),
        )
        .route(
            "/docs/next",
            get(|| async {
                Html("<html><body><main><h1>Next</h1><p>Second page.</p></main></body></html>")
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, handle)
}

#[tokio::test]
async fn add_local_then_sync_reports_summary() {
    let vault_dir = tempdir().expect("vault tempdir");
    let docs_dir = tempdir().expect("docs tempdir");
    std::fs::write(docs_dir.path().join("intro.md"), "# Intro\n\nHello.\n").expect("write");
    std::fs::write(docs_dir.path().join("guide.md"), "# Guide\n\nSteps.\n").expect("write");

    let lifecycle = SourceLifecycle::new(VaultConfig::new(vault_dir.path()));
    let added = lifecycle
        .add(
            AddSourceParams {
                origin: docs_dir.path().to_string_lossy().into_owned(),
                name: Some("notes".into()),
                ..Default::default()
            },
            true,
        )
        .await
        .expect("add local source");
    assert_eq!(added.source.kind, SourceKind::Local);
    assert_eq!(added.source.status, "ok");
    assert_eq!(added.summary.expect("summary").pages_added, 2);

    std::fs::write(docs_dir.path().join("extra.md"), "# Extra\n\nMore.\n").expect("write");
    let summary = synced(lifecycle.sync("notes", false).await.expect("sync"));
    assert_eq!(summary.pages_added, 1);
    assert_eq!(summary.pages_unchanged, 2);

    let all = lifecycle.sync_all(false).await.expect("sync_all");
    assert_eq!(all.len(), 1);
    let only = all.into_iter().next().expect("one result");
    assert_eq!(only.name, "notes");
    let summary = synced(only.result.expect("synced"));
    assert_eq!(summary.pages_unchanged, 3);
}

#[tokio::test]
async fn add_web_then_sync_via_loopback_fixture() {
    let (addr, server) = spawn_two_page_site().await;
    let vault_dir = tempdir().expect("vault tempdir");
    let lifecycle =
        SourceLifecycle::new(VaultConfig::new(vault_dir.path()).allowing_private_origins());

    let added = lifecycle
        .add(
            AddSourceParams {
                origin: format!("http://{addr}/docs/"),
                name: Some("fixture-docs".into()),
                ..Default::default()
            },
            true,
        )
        .await
        .expect("add web source");
    assert_eq!(added.source.kind, SourceKind::Web);
    assert_eq!(added.summary.expect("summary").pages_added, 2);

    let summary = synced(lifecycle.sync("fixture-docs", false).await.expect("resync"));
    server.abort();
    assert_eq!(summary.pages_unchanged, 2);
}

#[tokio::test]
async fn update_and_remove_roundtrip() {
    let vault_dir = tempdir().expect("vault tempdir");
    let lifecycle = SourceLifecycle::new(VaultConfig::new(vault_dir.path()));

    let added = lifecycle
        .add(
            AddSourceParams {
                origin: "https://example.com/docs".into(),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("register without sync");
    assert_eq!(added.source.name, "example-com-docs");
    assert!(added.summary.is_none());

    let updated = lifecycle
        .update(
            "example-com-docs",
            UpdateSourceParams {
                tags: Some(vec!["rust".into(), "rust".into(), " docs ".into()]),
                refresh_interval_secs: Some(0),
                ..Default::default()
            },
        )
        .expect("update");
    assert_eq!(updated.tags, vec!["rust", "docs"]);
    assert_eq!(updated.refresh_interval_secs, 0);

    let Removal::Removed(removed) = lifecycle.remove("example-com-docs").await.expect("remove")
    else {
        panic!("expected Removed");
    };
    assert_eq!(removed.name, "example-com-docs");
    assert_eq!(removed.pages_deleted, 0);

    let missing = lifecycle.remove("example-com-docs").await.unwrap_err();
    assert!(matches!(missing, VaultError::NotFound(_)));
}

#[tokio::test]
async fn operations_on_missing_vault_do_not_create_it() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path().join("absent"));
    let lifecycle = SourceLifecycle::new(config.clone());

    assert!(matches!(
        lifecycle.sync("nope", false).await.unwrap_err(),
        VaultError::NotFound(_)
    ));
    assert!(matches!(
        lifecycle.remove("nope").await.unwrap_err(),
        VaultError::NotFound(_)
    ));
    assert!(lifecycle.sync_all(false).await.expect("empty").is_empty());
    assert!(!config.db_path().exists());
}

#[tokio::test]
async fn add_rejects_private_origin_by_default() {
    let vault_dir = tempdir().expect("vault tempdir");
    let lifecycle = SourceLifecycle::new(VaultConfig::new(vault_dir.path()));
    for origin in [
        "http://127.0.0.1/docs",
        "http://10.0.0.1/",
        "http://docs.localhost/",
    ] {
        let err = lifecycle
            .add(
                AddSourceParams {
                    origin: origin.into(),
                    ..Default::default()
                },
                false,
            )
            .await
            .expect_err(origin);
        assert!(
            matches!(err, VaultError::Validation(_)),
            "{origin}: {err:?}"
        );
    }
}

#[tokio::test]
async fn add_allows_private_origin_when_enabled() {
    let vault_dir = tempdir().expect("vault tempdir");
    let lifecycle =
        SourceLifecycle::new(VaultConfig::new(vault_dir.path()).allowing_private_origins());
    let added = lifecycle
        .add(
            AddSourceParams {
                origin: "http://127.0.0.1:9/docs".into(),
                name: Some("intranet".into()),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("private origin allowed");
    assert_eq!(added.source.kind, SourceKind::Web);
}

#[tokio::test]
async fn sync_rejects_stored_private_origin_when_disabled() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path());
    let conn = crate::db::open_rw_db(&config.db_path()).expect("open db");
    conn.execute(
        "INSERT INTO sources (name, kind, origin) VALUES ('legacy', 'web', 'http://192.168.1.10/docs')",
        [],
    )
    .expect("insert legacy private source");
    drop(conn);

    let err = SourceLifecycle::new(config)
        .sync("legacy", false)
        .await
        .expect_err("stored private origin must be refused");
    assert!(matches!(err, VaultError::Validation(_)), "{err:?}");
}

#[tokio::test]
async fn concurrent_sync_returns_already_syncing() {
    let (addr, server) = spawn_slow_site(std::time::Duration::from_millis(400)).await;
    let vault_dir = tempdir().expect("vault tempdir");
    let lifecycle =
        SourceLifecycle::new(VaultConfig::new(vault_dir.path()).allowing_private_origins());
    lifecycle
        .add(
            AddSourceParams {
                origin: format!("http://{addr}/docs/"),
                name: Some("slow".into()),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("register");

    let (a, b) = tokio::join!(lifecycle.sync("slow", false), lifecycle.sync("slow", false));
    let outcomes = [a.expect("first"), b.expect("second")];
    let synced_count = outcomes
        .iter()
        .filter(|o| matches!(o, SyncOutcome::Synced(_)))
        .count();
    let refused = outcomes
        .iter()
        .filter(|o| matches!(o, SyncOutcome::AlreadySyncing))
        .count();
    assert_eq!((synced_count, refused), (1, 1), "{outcomes:?}");

    // The claim is released once the Sync finishes.
    assert!(matches!(
        lifecycle.sync("slow", false).await.expect("third"),
        SyncOutcome::Synced(_)
    ));
    server.abort();
}

#[tokio::test]
async fn remove_while_syncing_returns_already_syncing() {
    let vault_dir = tempdir().expect("vault tempdir");
    let docs_dir = tempdir().expect("docs tempdir");
    std::fs::write(docs_dir.path().join("a.md"), "# A\n\nAlpha.\n").expect("write");
    let lifecycle = SourceLifecycle::new(VaultConfig::new(vault_dir.path()));
    lifecycle
        .add(
            AddSourceParams {
                origin: docs_dir.path().to_string_lossy().into_owned(),
                name: Some("busy".into()),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("register");

    let held = lifecycle
        .claim("busy")
        .await
        .expect("claim")
        .expect("claim is free");
    assert!(matches!(
        lifecycle.remove("busy").await.expect("remove"),
        Removal::AlreadySyncing
    ));
    assert!(matches!(
        lifecycle.sync("busy", false).await.expect("sync"),
        SyncOutcome::AlreadySyncing
    ));

    let summary = held.run(false).await.expect("held sync runs");
    assert_eq!(summary.pages_added, 1);
    assert!(matches!(
        lifecycle.remove("busy").await.expect("remove"),
        Removal::Removed(_)
    ));
}

/// The Source shows the failure: status 'error', `last_error`, and an error Sync run on top.
fn assert_failure_recorded(config: &VaultConfig, name: &str) {
    let detail = crate::ingest::show_source(config, name).expect("show_source");
    assert_eq!(detail.source.status, "error", "{name}: {:?}", detail.source);
    assert!(detail.source.last_error.is_some(), "{name}: no last_error");
    let claims: i64 = crate::db::open_rw_db(&config.db_path())
        .expect("open db")
        .query_row(
            "SELECT COUNT(*) FROM sync_claims WHERE source_id = ?1",
            [detail.source.id],
            |r| r.get(0),
        )
        .expect("count claims");
    assert_eq!(claims, 0, "{name}: claim released after the failed Sync");
    let latest = detail.sync_runs.first().expect("a sync run was recorded");
    assert_eq!(latest.status, "error", "{name}: {latest:?}");
    assert!(latest.error_message.is_some(), "{name}: {latest:?}");
    assert!(latest.finished_at.is_some(), "{name}: {latest:?}");
    assert_eq!(
        detail
            .sync_runs
            .iter()
            .filter(|r| r.status == "running")
            .count(),
        0,
        "{name}: no run left running"
    );
}

async fn add_local(lifecycle: &SourceLifecycle, dir: &std::path::Path, name: &str) {
    lifecycle
        .add(
            AddSourceParams {
                origin: dir.to_string_lossy().into_owned(),
                name: Some(name.into()),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("register local source");
}

#[tokio::test]
async fn missing_local_folder_records_error_run() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path());
    let lifecycle = SourceLifecycle::new(config.clone());

    for name in ["via-sync", "via-claim"] {
        let docs_dir = tempdir().expect("docs tempdir");
        add_local(&lifecycle, docs_dir.path(), name).await;
        drop(docs_dir); // the folder disappears
    }

    lifecycle
        .sync("via-sync", false)
        .await
        .expect_err("missing folder must fail");
    assert_failure_recorded(&config, "via-sync");

    // The path the UI and scheduler take: claim, then run. (The watcher calls `sync`.)
    let claimed = lifecycle
        .claim("via-claim")
        .await
        .expect("claim")
        .expect("free");
    claimed
        .run(false)
        .await
        .expect_err("missing folder must fail");
    assert_failure_recorded(&config, "via-claim");
}

#[tokio::test]
async fn rejected_private_origin_records_error_run() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path());
    let conn = crate::db::open_rw_db(&config.db_path()).expect("open db");
    conn.execute(
        "INSERT INTO sources (name, kind, origin) VALUES ('legacy', 'web', 'http://10.1.2.3/docs')",
        [],
    )
    .expect("insert legacy private source");
    drop(conn);

    SourceLifecycle::new(config.clone())
        .sync("legacy", false)
        .await
        .expect_err("private origin refused");
    assert_failure_recorded(&config, "legacy");
}

#[tokio::test]
async fn add_with_sync_failure_records_error_run() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path()).allowing_private_origins();
    // Nothing listens on port 9 (discard), so the crawl fails.
    SourceLifecycle::new(config.clone())
        .add(
            AddSourceParams {
                origin: "http://127.0.0.1:9/docs/".into(),
                name: Some("unreachable".into()),
                ..Default::default()
            },
            true,
        )
        .await
        .expect_err("first sync fails");
    assert_failure_recorded(&config, "unreachable");
}

#[tokio::test]
async fn sync_all_continues_after_failure() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path());
    let lifecycle = SourceLifecycle::new(config.clone());

    let dirs: Vec<_> = (0..3).map(|_| tempdir().expect("docs tempdir")).collect();
    for (dir, name) in dirs.iter().zip(["a-first", "b-broken", "c-last"]) {
        std::fs::write(dir.path().join("p.md"), "# P\n\nPage.\n").expect("write");
        add_local(&lifecycle, dir.path(), name).await;
    }
    let mut dirs = dirs.into_iter();
    let (first, broken, last) = (dirs.next(), dirs.next(), dirs.next());
    drop(broken);

    let results = lifecycle.sync_all(false).await.expect("sync_all");
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["a-first", "b-broken", "c-last"]);
    assert!(matches!(results[0].result, Ok(SyncOutcome::Synced(_))));
    assert!(results[1].result.is_err());
    assert!(matches!(results[2].result, Ok(SyncOutcome::Synced(_))));
    assert_failure_recorded(&config, "b-broken");
    drop((first, last));
}

#[tokio::test]
async fn sync_all_skips_source_removed_mid_run() {
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path());
    let lifecycle = SourceLifecycle::new(config.clone());
    let docs = tempdir().expect("docs");
    std::fs::write(docs.path().join("p.md"), "# P\n\nPage.\n").expect("write");
    add_local(&lifecycle, docs.path(), "kept").await;

    // A claim attempt on a Source id that no longer exists is "not found", not a failure.
    let mut ghost = crate::ingest::load_source_by_name(
        &crate::db::open_rw_db(&config.db_path()).expect("open db"),
        "kept",
    )
    .expect("load");
    ghost.id += 1_000;
    let err = lifecycle
        .claim_record(ghost)
        .await
        .expect_err("ghost source");
    assert!(matches!(err, VaultError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn dropped_sync_future_still_finishes_in_background() {
    let (addr, server) = spawn_slow_site(std::time::Duration::from_millis(300)).await;
    let vault_dir = tempdir().expect("vault tempdir");
    let config = VaultConfig::new(vault_dir.path()).allowing_private_origins();
    let lifecycle = SourceLifecycle::new(config.clone());
    lifecycle
        .add(
            AddSourceParams {
                origin: format!("http://{addr}/docs/"),
                name: Some("slow".into()),
                ..Default::default()
            },
            false,
        )
        .await
        .expect("register");

    // The MCP HTTP transport: the request future is dropped when the client disconnects.
    let request = crate::mcp::handle_jsonrpc_payload(
        &config,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "manage", "arguments": {"action": "sync", "name": "slow"}}
        }),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(50), request).await;

    let mut status = String::new();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        status = crate::ingest::show_source(&config, "slow")
            .expect("show")
            .source
            .status;
        if status == "ok" {
            break;
        }
    }
    server.abort();
    assert_eq!(status, "ok", "sync must not be cancelled with its caller");
}
