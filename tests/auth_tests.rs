//! Integration tests for bearer token authentication middleware.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{json, Value};

use rustqueue::api::{self, AppState};
use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Start a test server with the given auth configuration.
/// Returns the HTTP base URL.
async fn start_test_server_with_auth(auth_config: AuthConfig) -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let qm = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager: qm,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config,
    });
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// ── Test 1: Auth disabled allows all requests ────────────────────────────────

#[tokio::test]
async fn test_auth_disabled_allows_all() {
    let base = start_test_server_with_auth(AuthConfig {
        enabled: false,
        tokens: vec![],
    })
    .await;

    let client = Client::new();

    // Health should work without a token.
    let resp = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // A protected endpoint (push a job) should also work without a token.
    let resp = client
        .post(format!("{base}/api/v1/queues/test/jobs"))
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

// ── Test 2: Auth enabled rejects requests with no token ──────────────────────

#[tokio::test]
async fn test_auth_enabled_rejects_no_token() {
    let base = start_test_server_with_auth(AuthConfig {
        enabled: true,
        tokens: vec!["secret-token".to_string()],
    })
    .await;

    let client = Client::new();

    // A protected endpoint without Authorization header should return 401.
    let resp = client
        .post(format!("{base}/api/v1/queues/test/jobs"))
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert!(body["error"]["message"].as_str().unwrap().contains("Missing"));
}

// ── Test 3: Auth enabled rejects requests with bad token ─────────────────────

#[tokio::test]
async fn test_auth_enabled_rejects_bad_token() {
    let base = start_test_server_with_auth(AuthConfig {
        enabled: true,
        tokens: vec!["correct-token".to_string()],
    })
    .await;

    let client = Client::new();

    // A protected endpoint with wrong token should return 401.
    let resp = client
        .post(format!("{base}/api/v1/queues/test/jobs"))
        .header("Authorization", "Bearer wrong-token")
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Invalid"));
}

// ── Test 4: Auth enabled accepts valid token ─────────────────────────────────

#[tokio::test]
async fn test_auth_enabled_accepts_valid_token() {
    let base = start_test_server_with_auth(AuthConfig {
        enabled: true,
        tokens: vec!["my-secret-token".to_string()],
    })
    .await;

    let client = Client::new();

    // A protected endpoint with the correct token should succeed.
    let resp = client
        .post(format!("{base}/api/v1/queues/test/jobs"))
        .header("Authorization", "Bearer my-secret-token")
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert!(body["id"].is_string());
}

// ── Test 5: Health endpoint is always public ─────────────────────────────────

#[tokio::test]
async fn test_health_always_public() {
    let base = start_test_server_with_auth(AuthConfig {
        enabled: true,
        tokens: vec!["secret".to_string()],
    })
    .await;

    let client = Client::new();

    // Health endpoint should be accessible even with auth enabled and no token.
    let resp = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "healthy");
}
