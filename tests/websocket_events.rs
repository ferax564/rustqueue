//! Integration tests for WebSocket event streaming.

use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

#[tokio::test]
async fn test_websocket_receives_push_event() {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(
        QueueManager::new(storage).with_event_sender(event_tx.clone()),
    );

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
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

    // Connect WebSocket
    let (mut ws_stream, _) = connect_async(format!("ws://{addr}/api/v1/events"))
        .await
        .expect("Failed to connect WebSocket");

    // Push a job via the QueueManager directly
    let id = queue_manager
        .push("emails", "send", json!({}), None)
        .await
        .unwrap();

    // Read the event
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        ws_stream.next(),
    )
    .await
    .expect("Timeout waiting for WS message")
    .expect("WS stream ended")
    .expect("WS error");

    let text = msg.to_text().unwrap();
    let event: Value = serde_json::from_str(text).unwrap();

    assert_eq!(event["event"], "job.pushed");
    assert_eq!(event["job_id"], id.to_string());
    assert_eq!(event["queue"], "emails");
    assert!(event["timestamp"].is_string());
}

#[tokio::test]
async fn test_websocket_receives_completed_event() {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(
        QueueManager::new(storage).with_event_sender(event_tx.clone()),
    );

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
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

    // Connect WebSocket
    let (mut ws_stream, _) = connect_async(format!("ws://{addr}/api/v1/events"))
        .await
        .expect("Failed to connect WebSocket");

    // Push, pull, and ack a job
    let id = queue_manager.push("work", "task", json!({}), None).await.unwrap();
    queue_manager.pull("work", 1).await.unwrap();

    // Drain the push event first
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Now ack
    queue_manager.ack(id, Some(json!({"done": true}))).await.unwrap();

    // Read the completed event
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .expect("Timeout waiting for completed event")
        .expect("WS stream ended")
        .expect("WS error");

    let text = msg.to_text().unwrap();
    let event: Value = serde_json::from_str(text).unwrap();

    assert_eq!(event["event"], "job.completed");
    assert_eq!(event["job_id"], id.to_string());
    assert_eq!(event["queue"], "work");
}

#[tokio::test]
async fn test_websocket_receives_cancelled_event() {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(
        QueueManager::new(storage).with_event_sender(event_tx.clone()),
    );

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
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

    // Connect WebSocket
    let (mut ws_stream, _) = connect_async(format!("ws://{addr}/api/v1/events"))
        .await
        .expect("Failed to connect WebSocket");

    // Push and cancel a job
    let id = queue_manager.push("work", "task", json!({}), None).await.unwrap();

    // Drain the push event
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Cancel the job
    queue_manager.cancel(id).await.unwrap();

    // Read the cancelled event
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .expect("Timeout waiting for cancelled event")
        .expect("WS stream ended")
        .expect("WS error");

    let text = msg.to_text().unwrap();
    let event: Value = serde_json::from_str(text).unwrap();

    assert_eq!(event["event"], "job.cancelled");
    assert_eq!(event["job_id"], id.to_string());
    assert_eq!(event["queue"], "work");
}

#[tokio::test]
async fn test_websocket_receives_failed_event_on_dlq() {
    use rustqueue::engine::queue::JobOptions;

    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(
        QueueManager::new(storage).with_event_sender(event_tx.clone()),
    );

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
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

    // Connect WebSocket
    let (mut ws_stream, _) = connect_async(format!("ws://{addr}/api/v1/events"))
        .await
        .expect("Failed to connect WebSocket");

    // Push a job with max_attempts=1 so failure goes directly to DLQ
    let opts = JobOptions {
        max_attempts: Some(1),
        ..Default::default()
    };
    let id = queue_manager
        .push("work", "fragile", json!({}), Some(opts))
        .await
        .unwrap();
    queue_manager.pull("work", 1).await.unwrap();

    // Drain the push event
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Fail the job (goes to DLQ since max_attempts=1)
    queue_manager.fail(id, "fatal error").await.unwrap();

    // Read the failed event
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .expect("Timeout waiting for failed event")
        .expect("WS stream ended")
        .expect("WS error");

    let text = msg.to_text().unwrap();
    let event: Value = serde_json::from_str(text).unwrap();

    assert_eq!(event["event"], "job.failed");
    assert_eq!(event["job_id"], id.to_string());
    assert_eq!(event["queue"], "work");
}

#[tokio::test]
async fn test_websocket_no_event_on_retry() {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(
        QueueManager::new(storage).with_event_sender(event_tx.clone()),
    );

    let state = Arc::new(AppState {
        queue_manager: Arc::clone(&queue_manager),
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

    // Connect WebSocket
    let (mut ws_stream, _) = connect_async(format!("ws://{addr}/api/v1/events"))
        .await
        .expect("Failed to connect WebSocket");

    // Push a job with default max_attempts=3 so failure triggers retry, not DLQ
    let id = queue_manager
        .push("work", "retryable", json!({}), None)
        .await
        .unwrap();
    queue_manager.pull("work", 1).await.unwrap();

    // Drain the push event
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Fail the job (should retry, NOT go to DLQ)
    let result = queue_manager.fail(id, "transient error").await.unwrap();
    assert!(result.will_retry, "job should be retried, not moved to DLQ");

    // There should be NO "job.failed" event because retries don't emit events
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        ws_stream.next(),
    )
    .await;

    assert!(
        timeout_result.is_err(),
        "expected no event on retry, but got one"
    );
}
