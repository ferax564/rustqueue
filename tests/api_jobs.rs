//! Integration tests for the HTTP REST API.

use std::sync::{Arc, OnceLock};

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use reqwest::Client;
use serde_json::{json, Value};

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::RedbStorage;

/// Install the Prometheus recorder exactly once across all tests in this binary.
fn global_metrics_handle() -> &'static PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        PrometheusBuilder::new()
            .install_recorder()
            .expect("failed to install Prometheus recorder")
    })
}

/// Start a test server on a random port and return its base URL.
/// The tempdir is leaked intentionally so it outlives the spawned server task.
async fn start_test_server() -> String {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
    // Leak the tempdir so it lives for the duration of the process.
    let _keep = Box::leak(Box::new(dir));

    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(RedbStorage::new(&db_path).unwrap());
    let qm = Arc::new(QueueManager::new(storage));
    let state = Arc::new(AppState {
        queue_manager: qm,
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

// ── Test 1: Push a job via HTTP ─────────────────────────────────────────────

#[tokio::test]
async fn test_push_job_via_http() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/queues/emails/jobs"))
        .json(&json!({
            "name": "send-welcome",
            "data": {"to": "user@example.com"}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert!(body["id"].is_string());
    // UUID should be parseable
    let id_str = body["id"].as_str().unwrap();
    uuid::Uuid::parse_str(id_str).expect("id should be a valid UUID");
}

// ── Test 2: Get a job via HTTP ──────────────────────────────────────────────

#[tokio::test]
async fn test_get_job_via_http() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push a job first.
    let push_resp = client
        .post(format!("{base}/api/v1/queues/emails/jobs"))
        .json(&json!({
            "name": "send-welcome",
            "data": {"to": "user@example.com"}
        }))
        .send()
        .await
        .unwrap();
    let push_body: Value = push_resp.json().await.unwrap();
    let id = push_body["id"].as_str().unwrap();

    // Get the job.
    let resp = client
        .get(format!("{base}/api/v1/jobs/{id}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["job"]["name"], "send-welcome");
    assert_eq!(body["job"]["queue"], "emails");
    assert_eq!(body["job"]["state"], "waiting");
}

// ── Test 3: Pull and Ack flow ───────────────────────────────────────────────

#[tokio::test]
async fn test_pull_ack_flow() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push a job.
    let push_resp = client
        .post(format!("{base}/api/v1/queues/work/jobs"))
        .json(&json!({
            "name": "process-data",
            "data": {"input": 42}
        }))
        .send()
        .await
        .unwrap();
    let push_body: Value = push_resp.json().await.unwrap();
    let id = push_body["id"].as_str().unwrap();

    // Pull the job.
    let pull_resp = client
        .get(format!("{base}/api/v1/queues/work/jobs"))
        .send()
        .await
        .unwrap();
    assert_eq!(pull_resp.status(), 200);
    let pull_body: Value = pull_resp.json().await.unwrap();
    assert_eq!(pull_body["ok"], true);
    assert!(pull_body["job"].is_object());
    assert_eq!(pull_body["job"]["id"], id);
    assert_eq!(pull_body["job"]["state"], "active");

    // Ack the job.
    let ack_resp = client
        .post(format!("{base}/api/v1/jobs/{id}/ack"))
        .json(&json!({"result": {"output": "done"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(ack_resp.status(), 200);
    let ack_body: Value = ack_resp.json().await.unwrap();
    assert_eq!(ack_body["ok"], true);

    // Verify job is now completed.
    let get_resp = client
        .get(format!("{base}/api/v1/jobs/{id}"))
        .send()
        .await
        .unwrap();
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["job"]["state"], "completed");
}

// ── Test 4: Health endpoint ─────────────────────────────────────────────────

#[tokio::test]
async fn test_health_endpoint() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/health"))
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

// ── Test 5: List queues ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_queues() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push jobs to two different queues.
    client
        .post(format!("{base}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "send", "data": {}}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/v1/queues/reports/jobs"))
        .json(&json!({"name": "generate", "data": {}}))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!("{base}/api/v1/queues"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let queues = body["queues"].as_array().unwrap();
    assert_eq!(queues.len(), 2);

    let names: Vec<&str> = queues.iter().map(|q| q["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"emails"));
    assert!(names.contains(&"reports"));
}

// ── Test 6: Queue stats ────────────────────────────────────────────────────

#[tokio::test]
async fn test_queue_stats() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push two jobs.
    client
        .post(format!("{base}/api/v1/queues/work/jobs"))
        .json(&json!({"name": "job1", "data": {}}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/v1/queues/work/jobs"))
        .json(&json!({"name": "job2", "data": {}}))
        .send()
        .await
        .unwrap();

    // Pull one job to make it active.
    client
        .get(format!("{base}/api/v1/queues/work/jobs?count=1"))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!("{base}/api/v1/queues/work/stats"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["counts"]["waiting"], 1);
    assert_eq!(body["counts"]["active"], 1);
}

// ── Test 7: Fail endpoint ───────────────────────────────────────────────────

#[tokio::test]
async fn test_fail_endpoint() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push and pull a job.
    let push_resp = client
        .post(format!("{base}/api/v1/queues/work/jobs"))
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();
    let push_body: Value = push_resp.json().await.unwrap();
    let id = push_body["id"].as_str().unwrap();

    client
        .get(format!("{base}/api/v1/queues/work/jobs"))
        .send()
        .await
        .unwrap();

    // Fail the job.
    let fail_resp = client
        .post(format!("{base}/api/v1/jobs/{id}/fail"))
        .json(&json!({"error": "timeout"}))
        .send()
        .await
        .unwrap();

    assert_eq!(fail_resp.status(), 200);
    let fail_body: Value = fail_resp.json().await.unwrap();
    assert_eq!(fail_body["ok"], true);
    assert_eq!(fail_body["retry"], true);
}

// ── Test 8: Cancel endpoint ─────────────────────────────────────────────────

#[tokio::test]
async fn test_cancel_endpoint() {
    let base = start_test_server().await;
    let client = Client::new();

    // Push a job (stays in Waiting state).
    let push_resp = client
        .post(format!("{base}/api/v1/queues/work/jobs"))
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();
    let push_body: Value = push_resp.json().await.unwrap();
    let id = push_body["id"].as_str().unwrap();

    // Cancel the job.
    let cancel_resp = client
        .post(format!("{base}/api/v1/jobs/{id}/cancel"))
        .send()
        .await
        .unwrap();

    assert_eq!(cancel_resp.status(), 200);
    let cancel_body: Value = cancel_resp.json().await.unwrap();
    assert_eq!(cancel_body["ok"], true);

    // Verify job is cancelled.
    let get_resp = client
        .get(format!("{base}/api/v1/jobs/{id}"))
        .send()
        .await
        .unwrap();
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["job"]["state"], "cancelled");
}

// ── Test 9: Batch push ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_batch_push() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/queues/batch/jobs"))
        .json(&json!([
            {"name": "job-a", "data": {"i": 1}},
            {"name": "job-b", "data": {"i": 2}},
            {"name": "job-c", "data": {"i": 3}}
        ]))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let ids = body["ids"].as_array().unwrap();
    assert_eq!(ids.len(), 3);

    // All IDs should be valid UUIDs.
    for id_val in ids {
        uuid::Uuid::parse_str(id_val.as_str().unwrap()).expect("id should be valid UUID");
    }
}

// ── Test 10: Not found returns 404 ──────────────────────────────────────────

#[tokio::test]
async fn test_not_found_returns_404() {
    let base = start_test_server().await;
    let client = Client::new();

    let fake_id = uuid::Uuid::now_v7();
    let resp = client
        .get(format!("{base}/api/v1/jobs/{fake_id}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "JOB_NOT_FOUND");
}

// ── Test 11: Error response format matches PRD ──────────────────────────────

#[tokio::test]
async fn test_error_response_format() {
    let base = start_test_server().await;
    let client = Client::new();

    let fake_id = uuid::Uuid::now_v7();
    let resp = client
        .get(format!("{base}/api/v1/jobs/{fake_id}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();

    // Verify exact structure matches PRD section 11.4:
    // { "ok": false, "error": { "code": "...", "message": "...", "details": null } }
    assert_eq!(body["ok"], false);
    assert!(body["error"].is_object(), "error should be an object");
    assert!(body["error"]["code"].is_string(), "error.code should be a string");
    assert!(body["error"]["message"].is_string(), "error.message should be a string");
    assert!(body["error"]["details"].is_null(), "error.details should be null");

    // Verify specific values.
    assert_eq!(body["error"]["code"], "JOB_NOT_FOUND");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&fake_id.to_string()),
        "error message should contain the job ID"
    );

    // Ensure there are no extra top-level keys.
    let obj = body.as_object().unwrap();
    assert_eq!(obj.len(), 2, "response should have exactly 2 keys: ok and error");

    // Ensure there are no extra keys in error.
    let error_obj = body["error"].as_object().unwrap();
    assert_eq!(error_obj.len(), 3, "error should have exactly 3 keys: code, message, details");
}

// ── Test 12: Prometheus metrics endpoint ─────────────────────────────────────

/// Start a test server with the Prometheus metrics handle wired in.
async fn start_test_server_with_metrics() -> String {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
    let _keep = Box::leak(Box::new(dir));

    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(RedbStorage::new(&db_path).unwrap());
    let qm = Arc::new(QueueManager::new(storage));
    let state = Arc::new(AppState {
        queue_manager: qm,
        start_time: std::time::Instant::now(),
        metrics_handle: Some(global_metrics_handle().clone()),
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
async fn test_prometheus_metrics_endpoint() {
    let base = start_test_server_with_metrics().await;
    let client = Client::new();

    // Push a job to generate some metrics.
    client
        .post(format!("{base}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!("{base}/api/v1/metrics/prometheus"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("rustqueue_jobs_pushed_total"));
}
