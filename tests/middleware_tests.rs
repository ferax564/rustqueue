//! Integration tests for Tower-HTTP middleware (CORS and request tracing).

use std::sync::Arc;

use reqwest::Client;
use serde_json::Value;

use rustqueue::api::{self, AppState};
use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Start a test server with default (auth-disabled) configuration.
/// Returns the HTTP base URL.
async fn start_test_server() -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let qm = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager: qm,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
    });
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// ── Test 1: CORS preflight returns appropriate headers ────────────────────────

#[tokio::test]
async fn test_cors_preflight_returns_headers() {
    let base = start_test_server().await;
    let client = Client::new();

    // Send an OPTIONS preflight request with the standard CORS headers.
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("{base}/api/v1/health"))
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "GET")
        .header("Access-Control-Request-Headers", "Content-Type")
        .send()
        .await
        .unwrap();

    // The response should include Access-Control-Allow-Origin.
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .expect("missing Access-Control-Allow-Origin header");
    assert_eq!(acao.to_str().unwrap(), "*");

    // It should also include Access-Control-Allow-Methods.
    assert!(
        resp.headers().get("access-control-allow-methods").is_some(),
        "missing Access-Control-Allow-Methods header"
    );

    // It should also include Access-Control-Allow-Headers.
    assert!(
        resp.headers().get("access-control-allow-headers").is_some(),
        "missing Access-Control-Allow-Headers header"
    );
}

// ── Test 2: Normal requests still work with middleware layers ─────────────────

#[tokio::test]
async fn test_requests_work_with_middleware() {
    let base = start_test_server().await;
    let client = Client::new();

    // A regular GET request to the health endpoint should still succeed.
    let resp = client
        .get(format!("{base}/api/v1/health"))
        .header("Origin", "https://example.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // CORS header should also be present on normal responses (not just preflight).
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .expect("missing Access-Control-Allow-Origin on normal response");
    assert_eq!(acao.to_str().unwrap(), "*");

    // Verify body is still correct.
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "healthy");
}
