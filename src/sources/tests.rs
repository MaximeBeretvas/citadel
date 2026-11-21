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
    assert_eq!(all[0].pages_unchanged, 3);
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
