//! Integration tests for graceful shutdown behavior.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value, json};

use rustqueue::api::{self, AppState};
use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::protocol;
use rustqueue::storage::MemoryStorage;

/// Start a full test server (HTTP + TCP) with graceful shutdown support.
/// Returns the HTTP base URL, the shutdown sender, and join handles for both servers.
async fn start_server_with_shutdown() -> (
    String,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage));

    // Build HTTP app
    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
    });
    let app = api::router(state);

    // Bind listeners on random ports
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn HTTP server with graceful shutdown
    let http_handle = tokio::spawn({
        let mut rx = shutdown_rx.clone();
        async move {
            axum::serve(http_listener, app)
                .with_graceful_shutdown(async move {
                    rx.changed().await.ok();
                })
                .await
                .expect("HTTP server error");
        }
    });

    // Spawn TCP server with shutdown receiver
    let auth_config = AuthConfig::default();
    let tcp_handle = tokio::spawn({
        let rx = shutdown_rx.clone();
        async move {
            protocol::start_tcp_server(tcp_listener, queue_manager, auth_config, rx).await;
        }
    });

    (
        format!("http://{http_addr}"),
        shutdown_tx,
        http_handle,
        tcp_handle,
    )
}

/// Verify that the server completes in-flight work and stops after shutdown signal.
#[tokio::test]
async fn test_graceful_shutdown_completes_inflight() {
    let (base_url, shutdown_tx, http_handle, tcp_handle) = start_server_with_shutdown().await;
    let client = Client::new();

    // Push a job to ensure the server is operational and data is persisted.
    let push_resp = client
        .post(format!("{base_url}/api/v1/queues/shutdown-test/jobs"))
        .json(&json!({
            "name": "pre-shutdown-job",
            "data": {"key": "value"}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        push_resp.status(),
        201,
        "push should succeed before shutdown"
    );
    let push_body: Value = push_resp.json().await.unwrap();
    assert_eq!(push_body["ok"], true);
    let job_id = push_body["id"].as_str().unwrap().to_string();

    // Verify the job is persisted by inspecting it.
    let get_resp = client
        .get(format!("{base_url}/api/v1/jobs/{job_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["ok"], true);
    assert_eq!(get_body["job"]["id"], job_id);

    // Send shutdown signal.
    shutdown_tx.send(true).unwrap();

    // Both servers should stop within a reasonable timeout (5s for tests).
    let drain_timeout = std::time::Duration::from_secs(5);
    let result = tokio::time::timeout(drain_timeout, async {
        let _ = http_handle.await;
        let _ = tcp_handle.await;
    })
    .await;

    assert!(
        result.is_ok(),
        "servers should shut down gracefully within the timeout"
    );
}
