use std::time::Duration;

use tempfile::tempdir;

#[tokio::test]
async fn detached_cli_refresh_populates_cache_in_child_process() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock github server");
    let addr = listener.local_addr().expect("local addr");
    let app = axum::Router::new().route(
        "/releases/latest",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "tag_name": "v0.9.0",
                "html_url": "https://github.com/mberetvas/citadel/releases/tag/v0.9.0"
            }))
        }),
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let vault_dir = tempdir().expect("vault tempdir");
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_citadel"));
    let api_url = format!("http://{addr}/releases/latest");

    citadel::update::check::spawn_detached_cli_refresh_with_url(bin, vault_dir.path(), &api_url);

    let cache_path = vault_dir.path().join("update-check.json");
    let mut found = false;
    for _ in 0..50 {
        if cache_path.is_file() {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    server.abort();

    assert!(
        found,
        "detached child process should have written {}",
        cache_path.display()
    );
    let raw = std::fs::read_to_string(&cache_path).expect("read cache");
    assert!(raw.contains("\"latest_version\": \"0.9.0\""), "{raw}");
}
