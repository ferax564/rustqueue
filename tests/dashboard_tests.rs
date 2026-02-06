//! Integration tests for the embedded dashboard asset serving.

use std::sync::Arc;

use reqwest::Client;

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Spin up a test HTTP server and return the base URL.
async fn start_test_server() -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: rustqueue::config::AuthConfig::default(),
    });
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn test_dashboard_index_returns_html() {
    let base_url = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/dashboard"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .expect("missing content-type header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "expected text/html content-type, got: {content_type}"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("RustQueue"),
        "expected body to contain 'RustQueue', got: {body}"
    );
}

#[tokio::test]
async fn test_landing_page_root_returns_html() {
    let base_url = start_test_server().await;
    let client = Client::new();

    let resp = client.get(&base_url).send().await.unwrap();

    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .expect("missing content-type header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "expected text/html content-type, got: {content_type}"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Queueing Infrastructure Without the") && body.contains("Ops Tax"),
        "expected landing page hero fragments, got: {body}"
    );
}

#[tokio::test]
async fn test_dashboard_missing_asset_returns_404() {
    let base_url = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/dashboard/nonexistent.js"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}
