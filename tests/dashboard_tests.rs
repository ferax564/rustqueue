//! Integration tests for the embedded dashboard asset serving.

use std::sync::Arc;

use reqwest::Client;

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Spin up a test HTTP server and return the base URL.
async fn start_test_server() -> String {
    start_test_server_with_auth(false).await
}

/// Spin up a test HTTP server with optional auth enabled and return the base URL.
async fn start_test_server_with_auth(auth_enabled: bool) -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage));

    let auth_config = if auth_enabled {
        rustqueue::config::AuthConfig {
            enabled: true,
            tokens: vec!["test-secret-token".to_string()],
        }
    } else {
        rustqueue::config::AuthConfig::default()
    };

    let state = Arc::new(AppState {
        queue_manager,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config,
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
        webhook_manager: None,
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

#[tokio::test]
async fn test_landing_page_accessible_when_auth_enabled() {
    let base_url = start_test_server_with_auth(true).await;
    let client = Client::new();

    // Landing page at `/` should be accessible WITHOUT a token even when auth is enabled.
    let resp = client.get(&base_url).send().await.unwrap();

    assert_eq!(
        resp.status(),
        200,
        "Landing page should be public even with auth enabled"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Queueing Infrastructure Without the") && body.contains("Ops Tax"),
        "expected landing page content"
    );
}

#[tokio::test]
async fn test_dashboard_requires_auth_when_enabled() {
    let base_url = start_test_server_with_auth(true).await;
    let client = Client::new();

    // Dashboard at `/dashboard` should require authentication when auth is enabled.
    let resp = client
        .get(format!("{base_url}/dashboard"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        401,
        "Dashboard should require auth when enabled"
    );
}
