//! End-to-end integration tests for the full schedule lifecycle.
//!
//! These tests verify that the background scheduler actively creates jobs from
//! schedules, and that the HTTP API correctly reflects execution counts, pause/resume
//! behaviour, and max_executions auto-pausing.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value, json};

use rustqueue::api::{self, AppState};
use rustqueue::config::{AuthConfig, RetentionConfig};
use rustqueue::engine::queue::QueueManager;
use rustqueue::engine::scheduler::start_scheduler;
use rustqueue::storage::MemoryStorage;

/// Start a test server with the background scheduler running at a fast tick rate.
///
/// Returns the base URL (e.g. `http://127.0.0.1:<port>`) and a `JoinHandle` for
/// the scheduler task so tests can abort it on cleanup.
async fn start_test_server_with_scheduler() -> (String, tokio::task::JoinHandle<()>) {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage).with_event_sender(event_tx.clone()));

    // Start scheduler with a fast 50ms tick so tests complete quickly.
    let scheduler_handle = start_scheduler(
        Arc::clone(&queue_manager),
        50,     // 50ms tick interval
        30_000, // stall timeout (irrelevant for these tests)
        RetentionConfig::default(),
    );

    let state = Arc::new(AppState {
        queue_manager,
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

    (format!("http://{addr}"), scheduler_handle)
}

/// Helper: pull up to `count` jobs from a queue via the HTTP API.
/// Returns the list of pulled jobs as JSON values.
async fn pull_jobs(client: &Client, base: &str, queue: &str, count: u32) -> Vec<Value> {
    let resp = client
        .get(format!("{base}/api/v1/queues/{queue}/jobs?count={count}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    body["jobs"].as_array().cloned().unwrap_or_default()
}

/// Helper: get a schedule by name via the HTTP API.
/// Returns the full response body as a JSON value.
async fn get_schedule(client: &Client, base: &str, name: &str) -> (u16, Value) {
    let resp = client
        .get(format!("{base}/api/v1/schedules/{name}"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    (status, body)
}

// ── Test 1: Interval schedule full lifecycle ─────────────────────────────────

#[tokio::test]
async fn test_interval_schedule_lifecycle() {
    let (base, scheduler) = start_test_server_with_scheduler().await;
    let client = Client::new();

    // Create an interval schedule that fires every 200ms.
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "fast-interval",
            "queue": "interval-q",
            "job_name": "interval-task",
            "job_data": {"source": "test"},
            "every_ms": 200
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        create_resp.status(),
        201,
        "creating schedule should return 201"
    );
    let create_body: Value = create_resp.json().await.unwrap();
    assert_eq!(create_body["ok"], true);
    assert_eq!(create_body["schedule"]["name"], "fast-interval");
    assert_eq!(create_body["schedule"]["every_ms"], 200);
    assert_eq!(create_body["schedule"]["execution_count"], 0);

    // Wait long enough for at least 2-3 firings:
    //  - first fire is immediate (next_run_at is None)
    //  - subsequent fires at ~200ms intervals
    // With 50ms scheduler tick, 700ms gives plenty of margin.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    // Verify execution_count >= 2 via GET.
    let (status, body) = get_schedule(&client, &base, "fast-interval").await;
    assert_eq!(status, 200);
    let exec_count = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert!(
        exec_count >= 2,
        "expected execution_count >= 2, got {exec_count}"
    );

    // Pull jobs from the queue — should have at least 2.
    let jobs = pull_jobs(&client, &base, "interval-q", 20).await;
    assert!(
        jobs.len() >= 2,
        "expected at least 2 jobs in interval-q, got {}",
        jobs.len()
    );
    // Verify job metadata matches the schedule.
    for job in &jobs {
        assert_eq!(job["name"], "interval-task");
        assert_eq!(job["queue"], "interval-q");
    }

    // Delete the schedule.
    let delete_resp = client
        .delete(format!("{base}/api/v1/schedules/fast-interval"))
        .send()
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), 200);

    // Verify GET now returns 404.
    let (status, body) = get_schedule(&client, &base, "fast-interval").await;
    assert_eq!(status, 404, "deleted schedule should return 404");
    assert_eq!(body["ok"], false);

    scheduler.abort();
}

// ── Test 2: Pause stops execution, resume restarts it ────────────────────────

#[tokio::test]
async fn test_schedule_pause_stops_execution() {
    let (base, scheduler) = start_test_server_with_scheduler().await;
    let client = Client::new();

    // Create a fast interval schedule (every 150ms).
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "pausable-sched",
            "queue": "pause-q",
            "job_name": "pause-task",
            "every_ms": 150
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    // Wait for some jobs to be created.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify some jobs were created.
    let (_, body) = get_schedule(&client, &base, "pausable-sched").await;
    let count_before_pause = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert!(
        count_before_pause >= 1,
        "expected at least 1 execution before pause, got {count_before_pause}"
    );

    // Pause the schedule.
    let pause_resp = client
        .post(format!("{base}/api/v1/schedules/pausable-sched/pause"))
        .send()
        .await
        .unwrap();
    assert_eq!(pause_resp.status(), 200);

    // Record current execution count.
    let (_, body) = get_schedule(&client, &base, "pausable-sched").await;
    let count_at_pause = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert_eq!(
        body["schedule"]["paused"], true,
        "schedule should be paused"
    );

    // Wait longer — no new jobs should be created while paused.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (_, body) = get_schedule(&client, &base, "pausable-sched").await;
    let count_after_wait = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    // Allow at most 1 extra due to a potential race between pause request and
    // an in-flight scheduler tick.
    assert!(
        count_after_wait <= count_at_pause + 1,
        "expected execution_count to not increase while paused (at pause: {count_at_pause}, after wait: {count_after_wait})"
    );

    // Resume the schedule.
    let resume_resp = client
        .post(format!("{base}/api/v1/schedules/pausable-sched/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resume_resp.status(), 200);

    // Wait for new jobs to be created after resume.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (_, body) = get_schedule(&client, &base, "pausable-sched").await;
    let count_after_resume = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert!(
        count_after_resume > count_after_wait,
        "expected new executions after resume (after wait: {count_after_wait}, after resume: {count_after_resume})"
    );

    scheduler.abort();
}

// ── Test 3: Cron schedule creates jobs ───────────────────────────────────────

#[tokio::test]
async fn test_cron_schedule_creates_jobs() {
    let (base, scheduler) = start_test_server_with_scheduler().await;
    let client = Client::new();

    // Create a cron schedule with `* * * * *` (every minute).
    // On the first scheduler tick, next_run_at is None so it fires immediately.
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "cron-sched",
            "queue": "cron-q",
            "job_name": "cron-task",
            "job_data": {"type": "cron"},
            "cron_expr": "* * * * *"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    // Wait for the first scheduler tick to fire the schedule.
    // With 50ms ticks, 300ms is generous.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Verify execution_count is at least 1 (first fire).
    let (status, body) = get_schedule(&client, &base, "cron-sched").await;
    assert_eq!(status, 200);
    let exec_count = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert!(
        exec_count >= 1,
        "cron schedule should have fired at least once, got execution_count={exec_count}"
    );

    // Verify next_run_at was computed (should be set after first execution).
    assert!(
        !body["schedule"]["next_run_at"].is_null(),
        "next_run_at should be set after the first cron execution"
    );

    // Verify last_run_at was set.
    assert!(
        !body["schedule"]["last_run_at"].is_null(),
        "last_run_at should be set after the first execution"
    );

    // Pull jobs and verify at least one was created.
    let jobs = pull_jobs(&client, &base, "cron-q", 10).await;
    assert!(
        !jobs.is_empty(),
        "cron schedule should have created at least one job"
    );
    assert_eq!(jobs[0]["name"], "cron-task");
    assert_eq!(jobs[0]["queue"], "cron-q");

    scheduler.abort();
}

// ── Test 4: max_executions auto-pauses the schedule ──────────────────────────

#[tokio::test]
async fn test_max_executions_auto_pauses() {
    let (base, scheduler) = start_test_server_with_scheduler().await;
    let client = Client::new();

    // Create a schedule with max_executions = 3 and fast interval (100ms).
    let create_resp = client
        .post(format!("{base}/api/v1/schedules"))
        .json(&json!({
            "name": "limited-sched",
            "queue": "limited-q",
            "job_name": "limited-task",
            "job_data": {"attempt": true},
            "every_ms": 100,
            "max_executions": 3
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    // Wait enough time for more than 3 potential firings.
    // 3 firings at 100ms intervals = ~300ms minimum. With 50ms scheduler ticks,
    // wait 800ms to be safe.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // Verify the schedule is auto-paused and execution_count == 3.
    let (status, body) = get_schedule(&client, &base, "limited-sched").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["schedule"]["paused"], true,
        "schedule should be auto-paused after reaching max_executions"
    );
    let exec_count = body["schedule"]["execution_count"].as_u64().unwrap_or(0);
    assert_eq!(
        exec_count, 3,
        "execution_count should be exactly 3 (max_executions limit), got {exec_count}"
    );

    // Pull jobs — should be exactly 3.
    let jobs = pull_jobs(&client, &base, "limited-q", 20).await;
    assert_eq!(
        jobs.len(),
        3,
        "expected exactly 3 jobs in limited-q, got {}",
        jobs.len()
    );
    for job in &jobs {
        assert_eq!(job["name"], "limited-task");
        assert_eq!(job["queue"], "limited-q");
    }

    scheduler.abort();
}
