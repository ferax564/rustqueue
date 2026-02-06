//! Full lifecycle integration test covering the complete job lifecycle through
//! the HTTP API: push, pull, ack, stats, health, failure/DLQ, delayed jobs,
//! and unique-key deduplication.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value, json};

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::RedbStorage;

/// Start a test server on a random port and return its base URL.
/// The tempdir is leaked intentionally so it outlives the spawned server task.
async fn start_test_server() -> String {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
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

#[tokio::test]
async fn test_full_job_lifecycle() {
    let base = start_test_server().await;
    let client = Client::new();

    // ── Step 1: Push a job via HTTP, verify 201 with job ID ─────────────

    let push_resp = client
        .post(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .json(&json!({
            "name": "process-order",
            "data": {"order_id": 1234, "amount": 99.99}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(push_resp.status(), 201, "push should return 201 Created");
    let push_body: Value = push_resp.json().await.unwrap();
    assert_eq!(push_body["ok"], true);
    let job_id = push_body["id"]
        .as_str()
        .expect("response should contain a job id");
    uuid::Uuid::parse_str(job_id).expect("id should be a valid UUID");

    // ── Step 2: Pull the job via HTTP, verify correct ID and Active state ─

    let pull_resp = client
        .get(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .send()
        .await
        .unwrap();

    assert_eq!(pull_resp.status(), 200);
    let pull_body: Value = pull_resp.json().await.unwrap();
    assert_eq!(pull_body["ok"], true);
    assert!(
        pull_body["job"].is_object(),
        "pull should return a job object"
    );
    assert_eq!(
        pull_body["job"]["id"], job_id,
        "pulled job should match pushed job id"
    );
    assert_eq!(
        pull_body["job"]["state"], "active",
        "pulled job should be in active state"
    );

    // ── Step 3: Ack the job, verify success ──────────────────────────────

    let ack_resp = client
        .post(format!("{base}/api/v1/jobs/{job_id}/ack"))
        .json(&json!({"result": null}))
        .send()
        .await
        .unwrap();

    assert_eq!(ack_resp.status(), 200, "ack should return 200 OK");
    let ack_body: Value = ack_resp.json().await.unwrap();
    assert_eq!(ack_body["ok"], true);

    // ── Step 4: Check queue stats — 0 waiting, 1 completed ──────────────

    let stats_resp = client
        .get(format!("{base}/api/v1/queues/lifecycle/stats"))
        .send()
        .await
        .unwrap();

    assert_eq!(stats_resp.status(), 200);
    let stats_body: Value = stats_resp.json().await.unwrap();
    assert_eq!(stats_body["ok"], true);
    assert_eq!(
        stats_body["counts"]["waiting"], 0,
        "no jobs should be waiting"
    );
    assert_eq!(
        stats_body["counts"]["completed"], 1,
        "one job should be completed"
    );

    // ── Step 5: Check health endpoint — 200 OK ──────────────────────────

    let health_resp = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        health_resp.status(),
        200,
        "health endpoint should return 200"
    );
    let health_body: Value = health_resp.json().await.unwrap();
    assert_eq!(health_body["ok"], true);
    assert_eq!(health_body["status"], "healthy");

    // ── Step 6: Failure -> DLQ flow ─────────────────────────────────────
    // Push a job with max_attempts=1, pull it, fail it, verify DLQ.

    let dlq_push_resp = client
        .post(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .json(&json!({
            "name": "fragile-task",
            "data": {"fragile": true},
            "max_attempts": 1
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(dlq_push_resp.status(), 201);
    let dlq_push_body: Value = dlq_push_resp.json().await.unwrap();
    let dlq_job_id = dlq_push_body["id"].as_str().unwrap();

    // Pull the fragile job.
    let dlq_pull_resp = client
        .get(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .send()
        .await
        .unwrap();

    assert_eq!(dlq_pull_resp.status(), 200);
    let dlq_pull_body: Value = dlq_pull_resp.json().await.unwrap();
    assert_eq!(dlq_pull_body["job"]["id"], dlq_job_id);

    // Fail the job — should go to DLQ since max_attempts=1.
    let fail_resp = client
        .post(format!("{base}/api/v1/jobs/{dlq_job_id}/fail"))
        .json(&json!({"error": "fatal crash"}))
        .send()
        .await
        .unwrap();

    assert_eq!(fail_resp.status(), 200);
    let fail_body: Value = fail_resp.json().await.unwrap();
    assert_eq!(fail_body["ok"], true);
    assert_eq!(
        fail_body["retry"], false,
        "with max_attempts=1 the job should not retry"
    );

    // Verify the job is now in DLQ state.
    let dlq_get_resp = client
        .get(format!("{base}/api/v1/jobs/{dlq_job_id}"))
        .send()
        .await
        .unwrap();

    let dlq_get_body: Value = dlq_get_resp.json().await.unwrap();
    assert_eq!(
        dlq_get_body["job"]["state"], "dlq",
        "job should be in DLQ after exhausting retries"
    );

    // ── Step 7: Delayed job — push with delay, pull immediately gets nothing

    let delayed_push_resp = client
        .post(format!("{base}/api/v1/queues/delayed-q/jobs"))
        .json(&json!({
            "name": "future-task",
            "data": {"scheduled": true},
            "delay_ms": 60000
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(delayed_push_resp.status(), 201);

    // Pull immediately — should get no jobs because it is delayed.
    let delayed_pull_resp = client
        .get(format!("{base}/api/v1/queues/delayed-q/jobs"))
        .send()
        .await
        .unwrap();

    assert_eq!(delayed_pull_resp.status(), 200);
    let delayed_pull_body: Value = delayed_pull_resp.json().await.unwrap();
    assert_eq!(delayed_pull_body["ok"], true);
    assert!(
        delayed_pull_body["job"].is_null(),
        "delayed job should not be pullable immediately"
    );

    // ── Step 8: Unique key deduplication ─────────────────────────────────
    // Push a job with unique_key, push again — second push should fail with 409.

    let unique_push1 = client
        .post(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .json(&json!({
            "name": "deduplicated-task",
            "data": {"attempt": 1},
            "unique_key": "order-42"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        unique_push1.status(),
        201,
        "first push with unique key should succeed"
    );

    let unique_push2 = client
        .post(format!("{base}/api/v1/queues/lifecycle/jobs"))
        .json(&json!({
            "name": "deduplicated-task",
            "data": {"attempt": 2},
            "unique_key": "order-42"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        unique_push2.status(),
        409,
        "second push with same unique key should return 409 Conflict"
    );
    let unique_push2_body: Value = unique_push2.json().await.unwrap();
    assert_eq!(unique_push2_body["ok"], false);
    assert_eq!(unique_push2_body["error"]["code"], "DUPLICATE_KEY");
}
