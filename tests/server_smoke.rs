//! Smoke tests for the full RustQueue server startup.

use std::sync::Arc;

use reqwest::Client;
use serde_json::Value;

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::{MemoryStorage, RedbStorage};

/// Start a full test server (HTTP + TCP) on random ports.
/// Returns the HTTP base URL and TCP address.
async fn start_full_server() -> (String, std::net::SocketAddr) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("smoke.redb");
    // Leak the tempdir so it outlives the spawned server tasks.
    let _keep = Box::leak(Box::new(dir));

    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(RedbStorage::new(&db_path).unwrap());
    let queue_manager = Arc::new(QueueManager::new(storage));

    // Build HTTP app
    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: rustqueue::config::AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
        webhook_manager: None,
    });
    let app = api::router(state);

    // Bind HTTP listener on random port
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();

    // Bind TCP listener on random port
    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_addr = tcp_listener.local_addr().unwrap();

    // Spawn HTTP server
    tokio::spawn(async move {
        axum::serve(http_listener, app).await.unwrap();
    });

    // Spawn TCP server (auth disabled for smoke tests)
    let auth_config = rustqueue::config::AuthConfig::default();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // Leak the sender so it outlives the spawned server task (never sends shutdown).
    let _keep_tx = Box::leak(Box::new(shutdown_tx));
    tokio::spawn(async move {
        rustqueue::protocol::start_tcp_server(
            tcp_listener,
            queue_manager,
            auth_config,
            shutdown_rx,
        )
        .await;
    });

    (format!("http://{http_addr}"), tcp_addr)
}

#[tokio::test]
async fn test_server_starts_and_responds_to_health() {
    let (base_url, _tcp_addr) = start_full_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/api/v1/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["version"], "0.1.0");
    assert!(body["uptime_seconds"].is_number());
}

#[tokio::test]
async fn test_server_cli_help() {
    #[allow(deprecated)]
    let mut cmd = assert_cmd::Command::cargo_bin("rustqueue").unwrap();
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("Background jobs without infrastructure"));
}

#[tokio::test]
async fn test_cli_status_help() {
    #[allow(deprecated)]
    let mut cmd = assert_cmd::Command::cargo_bin("rustqueue").unwrap();
    cmd.arg("status").arg("--help");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("queue status"));
}

#[tokio::test]
async fn test_server_with_memory_backend() {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: rustqueue::config::AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
        webhook_manager: None,
    });
    let app = api::router(state);

    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(http_listener, app).await.unwrap();
    });

    let client = Client::new();
    let resp = client
        .get(format!("http://{http_addr}/api/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
}
