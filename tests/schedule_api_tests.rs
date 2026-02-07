//! Integration tests for the schedule HTTP API endpoints.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value, json};

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Start a test server on a random port and return its base URL.
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

// ── Test 1: Create and get a schedule ────────────────────────────────────────

#[tokio::test]
async fn test_create_and_get_schedule() {
    let base = start_test_server().await;
    let client = Client::new();

    // Create a schedule.
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "daily-report",
            "queue": "reports",
            "job_name": "generate-report",
            "job_data": {"format": "pdf"},
            "cron_expr": "0 9 * * *"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(create_resp.status(), 201);
    let create_body: Value = create_resp.json().await.unwrap();
    assert_eq!(create_body["ok"], true);
    assert_eq!(create_body["schedule"]["name"], "daily-report");
    assert_eq!(create_body["schedule"]["queue"], "reports");
    assert_eq!(create_body["schedule"]["job_name"], "generate-report");
    assert_eq!(create_body["schedule"]["cron_expr"], "0 9 * * *");
    assert_eq!(create_body["schedule"]["paused"], false);
    assert_eq!(create_body["schedule"]["execution_count"], 0);

    // Get the schedule by name.
    let get_resp = client
        .get(format!("{base}/api/v1/schedules/daily-report"))
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), 200);
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["ok"], true);
    assert_eq!(get_body["schedule"]["name"], "daily-report");
    assert_eq!(get_body["schedule"]["queue"], "reports");
    assert_eq!(get_body["schedule"]["job_name"], "generate-report");
    assert_eq!(get_body["schedule"]["job_data"]["format"], "pdf");
}

// ── Test 2: List schedules ──────────────────────────────────────────────────

#[tokio::test]
async fn test_list_schedules() {
    let base = start_test_server().await;
    let client = Client::new();

    // Create two schedules.
    client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "schedule-a",
            "queue": "q1",
            "job_name": "job-a",
            "every_ms": 60000
        }))
        .send()
        .await
        .unwrap();

    client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "schedule-b",
            "queue": "q2",
            "job_name": "job-b",
            "cron_expr": "*/5 * * * *"
        }))
        .send()
        .await
        .unwrap();

    // List all schedules.
    let list_resp = client
        .get(format!("{base}/api/v1/schedules"))
        .send()
        .await
        .unwrap();

    assert_eq!(list_resp.status(), 200);
    let list_body: Value = list_resp.json().await.unwrap();
    assert_eq!(list_body["ok"], true);
    let schedules = list_body["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 2);

    let names: Vec<&str> = schedules
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"schedule-a"));
    assert!(names.contains(&"schedule-b"));
}

// ── Test 3: Delete a schedule ───────────────────────────────────────────────

#[tokio::test]
async fn test_delete_schedule() {
    let base = start_test_server().await;
    let client = Client::new();

    // Create a schedule.
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "to-delete",
            "queue": "work",
            "job_name": "task",
            "every_ms": 30000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    // Delete the schedule.
    let delete_resp = client
        .delete(format!("{base}/api/v1/schedules/to-delete"))
        .send()
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), 200);
    let delete_body: Value = delete_resp.json().await.unwrap();
    assert_eq!(delete_body["ok"], true);

    // Verify GET now returns 404.
    let get_resp = client
        .get(format!("{base}/api/v1/schedules/to-delete"))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 404);
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["ok"], false);
    assert_eq!(get_body["error"]["code"], "SCHEDULE_NOT_FOUND");
}

// ── Test 4: Pause and resume a schedule ─────────────────────────────────────

#[tokio::test]
async fn test_pause_and_resume() {
    let base = start_test_server().await;
    let client = Client::new();

    // Create a schedule.
    client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "pausable",
            "queue": "work",
            "job_name": "task",
            "cron_expr": "0 * * * *"
        }))
        .send()
        .await
        .unwrap();

    // Pause the schedule.
    let pause_resp = client
        .post(format!("{base}/api/v1/schedules/pausable/pause"))
        .send()
        .await
        .unwrap();
    assert_eq!(pause_resp.status(), 200);
    let pause_body: Value = pause_resp.json().await.unwrap();
    assert_eq!(pause_body["ok"], true);

    // Verify it's paused.
    let get_resp = client
        .get(format!("{base}/api/v1/schedules/pausable"))
        .send()
        .await
        .unwrap();
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["schedule"]["paused"], true);

    // Resume the schedule.
    let resume_resp = client
        .post(format!("{base}/api/v1/schedules/pausable/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resume_resp.status(), 200);
    let resume_body: Value = resume_resp.json().await.unwrap();
    assert_eq!(resume_body["ok"], true);

    // Verify it's resumed.
    let get_resp = client
        .get(format!("{base}/api/v1/schedules/pausable"))
        .send()
        .await
        .unwrap();
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["schedule"]["paused"], false);
}

// ── Test 5: Invalid cron expression returns 400 ─────────────────────────────

#[tokio::test]
async fn test_create_invalid_cron_returns_400() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "bad-cron",
            "queue": "work",
            "job_name": "task",
            "cron_expr": "not a valid cron"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}
