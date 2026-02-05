# Phase 1: Foundation — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a working job queue with HTTP and TCP APIs, redb storage, and full test coverage — delivering the v0.1 milestone from the PRD.

**Architecture:** Core engine manages job state transitions through the `StorageBackend` trait backed by redb. HTTP API via axum exposes REST endpoints. TCP protocol uses tokio raw TCP with newline-delimited JSON. Configuration via clap + config-rs with layered priority.

**Tech Stack:** Rust 2024, tokio, axum, redb, serde_json, clap, tracing, uuid v7, chrono, croner

---

## Task 1: Configuration Module

**Files:**
- Create: `src/config/mod.rs` (replace stub)
- Test: inline `#[cfg(test)]` module

**Step 1: Write the failing test**

```rust
// In src/config/mod.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RustQueueConfig::default();
        assert_eq!(config.server.http_port, 6790);
        assert_eq!(config.server.tcp_port, 6789);
        assert_eq!(config.storage.backend, StorageBackendType::Redb);
        assert_eq!(config.jobs.default_max_attempts, 3);
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config = RustQueueConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: RustQueueConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.server.http_port, config.server.http_port);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test config::tests -- -v`
Expected: FAIL — `RustQueueConfig` not defined

**Step 3: Write minimal implementation**

```rust
// src/config/mod.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustQueueConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub http_port: u16,
    pub tcp_port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            http_port: 6790,
            tcp_port: 6789,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackendType {
    Redb,
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub backend: StorageBackendType,
    pub path: String,
    pub postgres_url: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackendType::Redb,
            path: "./data".into(),
            postgres_url: None,
        }
    }
}

// ... remaining config structs with defaults (AuthConfig, SchedulerConfig,
// JobsConfig, RetentionConfig, DashboardConfig, LoggingConfig, MetricsConfig)
// Each with sensible defaults matching PRD section 13.2
```

**Step 4: Run test to verify it passes**

Run: `cargo test config::tests -- -v`
Expected: PASS (2 tests)

**Step 5: Commit**

```bash
git add src/config/
git commit -m "feat(config): add configuration module with layered defaults"
```

---

## Task 2: redb Storage Backend — Setup and Job Insert/Get

**Files:**
- Create: `src/storage/redb.rs`
- Modify: `src/storage/mod.rs` — add `pub mod redb;`
- Test: inline `#[cfg(test)]` in `src/storage/redb.rs`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Job;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_insert_and_get_job() {
        let dir = tempdir().unwrap();
        let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();
        let job = Job::new("emails", "send-welcome", serde_json::json!({"to": "a@b.com"}));
        let id = job.id;

        store.insert_job(&job).await.unwrap();
        let retrieved = store.get_job(id).await.unwrap().unwrap();

        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.queue, "emails");
        assert_eq!(retrieved.name, "send-welcome");
    }

    #[tokio::test]
    async fn test_get_nonexistent_job_returns_none() {
        let dir = tempdir().unwrap();
        let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();
        let result = store.get_job(uuid::Uuid::now_v7()).await.unwrap();
        assert!(result.is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test storage::redb::tests -- -v`
Expected: FAIL — `RedbStorage` not defined

**Step 3: Write minimal implementation**

```rust
// src/storage/redb.rs
use std::path::Path;
use redb::{Database, TableDefinition};
use crate::engine::models::{Job, JobId, QueueCounts, Schedule};
use super::StorageBackend;

const JOBS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("jobs");

pub struct RedbStorage {
    db: Database,
}

impl RedbStorage {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let db = Database::create(path)?;
        // Create tables on first open
        let write_txn = db.begin_write()?;
        { let _ = write_txn.open_table(JOBS_TABLE)?; }
        write_txn.commit()?;
        Ok(Self { db })
    }
}

#[async_trait::async_trait]
impl StorageBackend for RedbStorage {
    async fn insert_job(&self, job: &Job) -> anyhow::Result<JobId> {
        let key = job.id.as_bytes().to_vec();
        let value = serde_json::to_vec(job)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.insert(key.as_slice(), value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(job.id)
    }

    async fn get_job(&self, id: JobId) -> anyhow::Result<Option<Job>> {
        let key = id.as_bytes().to_vec();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;
        match table.get(key.as_slice())? {
            Some(value) => {
                let job: Job = serde_json::from_slice(value.value())?;
                Ok(Some(job))
            }
            None => Ok(None),
        }
    }
    // ... remaining trait methods stubbed with todo!()
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test storage::redb::tests -- -v`
Expected: PASS (2 tests)

**Step 5: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): add redb backend with job insert/get"
```

---

## Task 3: redb Storage — Update, Delete, and Dequeue

**Files:**
- Modify: `src/storage/redb.rs` — implement `update_job`, `delete_job`, `dequeue`
- Test: add tests in `src/storage/redb.rs`

**Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn test_update_job() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();
    let mut job = Job::new("emails", "send-welcome", serde_json::json!({}));
    store.insert_job(&job).await.unwrap();

    job.state = JobState::Active;
    job.attempt = 1;
    store.update_job(&job).await.unwrap();

    let updated = store.get_job(job.id).await.unwrap().unwrap();
    assert_eq!(updated.state, JobState::Active);
    assert_eq!(updated.attempt, 1);
}

#[tokio::test]
async fn test_delete_job() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();
    let job = Job::new("emails", "send-welcome", serde_json::json!({}));
    store.insert_job(&job).await.unwrap();
    store.delete_job(job.id).await.unwrap();
    assert!(store.get_job(job.id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_dequeue_returns_waiting_jobs_fifo() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let job1 = Job::new("emails", "j1", serde_json::json!({}));
    let job2 = Job::new("emails", "j2", serde_json::json!({}));
    store.insert_job(&job1).await.unwrap();
    store.insert_job(&job2).await.unwrap();

    let dequeued = store.dequeue("emails", 1).await.unwrap();
    assert_eq!(dequeued.len(), 1);
    assert_eq!(dequeued[0].id, job1.id); // FIFO: first inserted
    assert_eq!(dequeued[0].state, JobState::Active); // State transitioned
}

#[tokio::test]
async fn test_dequeue_empty_queue_returns_empty() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();
    let dequeued = store.dequeue("nonexistent", 5).await.unwrap();
    assert!(dequeued.is_empty());
}

#[tokio::test]
async fn test_dequeue_respects_priority() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let mut low = Job::new("q", "low", serde_json::json!({}));
    low.priority = 1;
    let mut high = Job::new("q", "high", serde_json::json!({}));
    high.priority = 10;

    store.insert_job(&low).await.unwrap();
    store.insert_job(&high).await.unwrap();

    let dequeued = store.dequeue("q", 1).await.unwrap();
    assert_eq!(dequeued[0].name, "high"); // Higher priority first
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test storage::redb::tests -- -v`
Expected: FAIL on the new tests

**Step 3: Implement `update_job`, `delete_job`, `dequeue`**

The `dequeue` implementation must:
1. Scan all jobs in the queue with `state == Waiting`
2. Sort by priority (descending), then by `created_at` (ascending, FIFO tiebreaker)
3. Take up to `count` jobs
4. Transition each to `Active` state and set `started_at`
5. Write updates in a single transaction

For redb, this requires a secondary index. Use a separate table `QUEUE_INDEX` mapping `(queue, state, priority, created_at) -> job_id` for efficient lookups. Alternatively, for v0.1, scan JOBS_TABLE and filter (simpler, slower, optimize later).

**Step 4: Run tests to verify they pass**

Run: `cargo test storage::redb::tests -- -v`
Expected: PASS (7 tests total)

**Step 5: Commit**

```bash
git add src/storage/redb.rs
git commit -m "feat(storage): add update, delete, dequeue with priority ordering"
```

---

## Task 4: redb Storage — Queue Counts, DLQ, Scheduled Jobs, Schedules

**Files:**
- Modify: `src/storage/redb.rs` — implement remaining `StorageBackend` methods
- Test: add tests in `src/storage/redb.rs`

**Step 1: Write failing tests**

```rust
#[tokio::test]
async fn test_queue_counts() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let job1 = Job::new("q", "j1", serde_json::json!({})); // state: Waiting
    let mut job2 = Job::new("q", "j2", serde_json::json!({}));
    job2.state = JobState::Active;
    store.insert_job(&job1).await.unwrap();
    store.insert_job(&job2).await.unwrap();

    let counts = store.get_queue_counts("q").await.unwrap();
    assert_eq!(counts.waiting, 1);
    assert_eq!(counts.active, 1);
}

#[tokio::test]
async fn test_move_to_dlq_and_retrieve() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let job = Job::new("q", "j1", serde_json::json!({}));
    store.insert_job(&job).await.unwrap();
    store.move_to_dlq(&job, "max retries exceeded").await.unwrap();

    let dlq_jobs = store.get_dlq_jobs("q", 10).await.unwrap();
    assert_eq!(dlq_jobs.len(), 1);
    assert_eq!(dlq_jobs[0].state, JobState::Dlq);
}

#[tokio::test]
async fn test_scheduled_jobs_ready() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let mut job = Job::new("q", "j1", serde_json::json!({}));
    job.state = JobState::Delayed;
    job.delay_until = Some(chrono::Utc::now() - chrono::Duration::seconds(10)); // past
    store.insert_job(&job).await.unwrap();

    let ready = store.get_ready_scheduled(chrono::Utc::now()).await.unwrap();
    assert_eq!(ready.len(), 1);
}

#[tokio::test]
async fn test_upsert_and_get_schedules() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let schedule = Schedule {
        name: "daily-report".into(),
        queue: "reports".into(),
        job_name: "generate-report".into(),
        job_data: serde_json::json!({}),
        cron_expr: Some("0 0 * * *".into()),
        every_ms: None,
        timezone: None,
        max_executions: None,
        execution_count: 0,
        paused: false,
        last_run_at: None,
        next_run_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    store.upsert_schedule(&schedule).await.unwrap();
    let schedules = store.get_active_schedules().await.unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].name, "daily-report");
}

#[tokio::test]
async fn test_remove_completed_before() {
    let dir = tempdir().unwrap();
    let store = RedbStorage::new(dir.path().join("test.redb")).unwrap();

    let mut job = Job::new("q", "old", serde_json::json!({}));
    job.state = JobState::Completed;
    job.completed_at = Some(chrono::Utc::now() - chrono::Duration::days(30));
    store.insert_job(&job).await.unwrap();

    let removed = store.remove_completed_before(
        chrono::Utc::now() - chrono::Duration::days(7)
    ).await.unwrap();
    assert_eq!(removed, 1);
}
```

**Step 2: Run tests — expect failures**

**Step 3: Implement all remaining StorageBackend methods**

**Step 4: Run all storage tests**

Run: `cargo test storage:: -- -v`
Expected: PASS (12+ tests)

**Step 5: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): complete redb StorageBackend implementation"
```

---

## Task 5: Queue Manager — Core Engine

**Files:**
- Create: `src/engine/queue.rs`
- Modify: `src/engine/mod.rs` — add `pub mod queue;`
- Test: inline `#[cfg(test)]` in `src/engine/queue.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::redb::RedbStorage;
    use tempfile::tempdir;
    use std::sync::Arc;

    async fn test_queue_manager() -> QueueManager {
        let dir = tempdir().unwrap();
        let storage = Arc::new(RedbStorage::new(dir.path().join("test.redb")).unwrap());
        QueueManager::new(storage)
    }

    #[tokio::test]
    async fn test_push_job() {
        let qm = test_queue_manager().await;
        let id = qm.push("emails", "send-welcome", serde_json::json!({"to": "a@b.com"}), None).await.unwrap();
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Waiting);
    }

    #[tokio::test]
    async fn test_push_and_pull() {
        let qm = test_queue_manager().await;
        qm.push("emails", "j1", serde_json::json!({}), None).await.unwrap();
        let jobs = qm.pull("emails", 1).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Active);
    }

    #[tokio::test]
    async fn test_ack_job() {
        let qm = test_queue_manager().await;
        let id = qm.push("q", "j", serde_json::json!({}), None).await.unwrap();
        let jobs = qm.pull("q", 1).await.unwrap();
        qm.ack(jobs[0].id, Some(serde_json::json!({"ok": true}))).await.unwrap();
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Completed);
    }

    #[tokio::test]
    async fn test_fail_job_with_retries() {
        let qm = test_queue_manager().await;
        let id = qm.push("q", "j", serde_json::json!({}), None).await.unwrap();
        qm.pull("q", 1).await.unwrap();
        let result = qm.fail(id, "SMTP timeout").await.unwrap();
        assert!(result.will_retry);
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Waiting); // Back in queue for retry
        assert_eq!(job.attempt, 1);
    }

    #[tokio::test]
    async fn test_fail_job_exhausted_retries_goes_to_dlq() {
        let qm = test_queue_manager().await;
        let mut opts = JobOptions::default();
        opts.max_attempts = Some(1); // Only 1 attempt allowed
        let id = qm.push("q", "j", serde_json::json!({}), Some(opts)).await.unwrap();
        qm.pull("q", 1).await.unwrap();
        let result = qm.fail(id, "permanent error").await.unwrap();
        assert!(!result.will_retry);
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Dlq);
    }

    #[tokio::test]
    async fn test_cancel_waiting_job() {
        let qm = test_queue_manager().await;
        let id = qm.push("q", "j", serde_json::json!({}), None).await.unwrap();
        qm.cancel(id).await.unwrap();
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_active_job_fails() {
        let qm = test_queue_manager().await;
        let id = qm.push("q", "j", serde_json::json!({}), None).await.unwrap();
        qm.pull("q", 1).await.unwrap(); // Now active
        let result = qm.cancel(id).await;
        assert!(result.is_err()); // Can't cancel active jobs
    }

    #[tokio::test]
    async fn test_unique_key_deduplication() {
        let qm = test_queue_manager().await;
        let mut opts = JobOptions::default();
        opts.unique_key = Some("user-123".into());
        qm.push("q", "j1", serde_json::json!({}), Some(opts.clone())).await.unwrap();
        let result = qm.push("q", "j2", serde_json::json!({}), Some(opts)).await;
        assert!(result.is_err()); // Duplicate key
    }

    #[tokio::test]
    async fn test_delayed_job() {
        let qm = test_queue_manager().await;
        let mut opts = JobOptions::default();
        opts.delay_ms = Some(60_000); // 1 minute in the future
        let id = qm.push("q", "j", serde_json::json!({}), Some(opts)).await.unwrap();
        let job = qm.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Delayed);

        // Should not be dequeued
        let jobs = qm.pull("q", 1).await.unwrap();
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn test_list_queues() {
        let qm = test_queue_manager().await;
        qm.push("emails", "j1", serde_json::json!({}), None).await.unwrap();
        qm.push("reports", "j2", serde_json::json!({}), None).await.unwrap();
        let queues = qm.list_queues().await.unwrap();
        assert!(queues.len() >= 2);
    }
}
```

**Step 2: Run tests — expect failures**

**Step 3: Implement QueueManager**

```rust
// src/engine/queue.rs
use std::sync::Arc;
use crate::engine::models::*;
use crate::storage::StorageBackend;

pub struct QueueManager {
    storage: Arc<dyn StorageBackend>,
}

#[derive(Debug, Clone, Default)]
pub struct JobOptions {
    pub priority: Option<i32>,
    pub delay_ms: Option<u64>,
    pub max_attempts: Option<u32>,
    pub backoff: Option<BackoffStrategy>,
    pub backoff_delay_ms: Option<u64>,
    pub ttl_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub unique_key: Option<String>,
    pub tags: Option<Vec<String>>,
    pub group_id: Option<String>,
    pub lifo: Option<bool>,
    pub remove_on_complete: Option<bool>,
    pub remove_on_fail: Option<bool>,
}

#[derive(Debug)]
pub struct FailResult {
    pub will_retry: bool,
    pub next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl QueueManager {
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self { ... }
    pub async fn push(&self, queue: &str, name: &str, data: serde_json::Value, opts: Option<JobOptions>) -> anyhow::Result<JobId> { ... }
    pub async fn pull(&self, queue: &str, count: u32) -> anyhow::Result<Vec<Job>> { ... }
    pub async fn ack(&self, id: JobId, result: Option<serde_json::Value>) -> anyhow::Result<()> { ... }
    pub async fn fail(&self, id: JobId, error: &str) -> anyhow::Result<FailResult> { ... }
    pub async fn cancel(&self, id: JobId) -> anyhow::Result<()> { ... }
    pub async fn get_job(&self, id: JobId) -> anyhow::Result<Option<Job>> { ... }
    pub async fn list_queues(&self) -> anyhow::Result<Vec<QueueInfo>> { ... }
    pub async fn get_queue_stats(&self, queue: &str) -> anyhow::Result<QueueCounts> { ... }
}
```

**Step 4: Run all engine tests**

Run: `cargo test engine:: -- -v`
Expected: PASS (10+ tests)

**Step 5: Commit**

```bash
git add src/engine/
git commit -m "feat(engine): add QueueManager with push/pull/ack/fail/cancel"
```

---

## Task 6: Error Types

**Files:**
- Create: `src/engine/error.rs`
- Modify: `src/engine/mod.rs` — add `pub mod error;`

**Step 1: Write failing test**

```rust
#[test]
fn test_error_codes_serialize() {
    let err = RustQueueError::QueueNotFound("emails".into());
    assert_eq!(err.error_code(), "QUEUE_NOT_FOUND");
    assert_eq!(err.http_status(), 404);
}
```

**Step 2: Run test — expect failure**

**Step 3: Implement error types**

```rust
// src/engine/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RustQueueError {
    #[error("Queue '{0}' not found")]
    QueueNotFound(String),

    #[error("Job '{0}' not found")]
    JobNotFound(String),

    #[error("Job is in invalid state '{current}' for operation (expected: {expected})")]
    InvalidState { current: String, expected: String },

    #[error("Duplicate unique key '{0}'")]
    DuplicateKey(String),

    #[error("Queue '{0}' is paused")]
    QueuePaused(String),

    #[error("Rate limit exceeded")]
    RateLimited,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl RustQueueError {
    pub fn error_code(&self) -> &'static str { ... }
    pub fn http_status(&self) -> u16 { ... }
}
```

**Step 4: Run tests — expect pass**

**Step 5: Commit**

```bash
git add src/engine/error.rs src/engine/mod.rs
git commit -m "feat(engine): add typed error codes matching PRD error spec"
```

---

## Task 7: HTTP API — Core Job Endpoints

**Files:**
- Create: `src/api/mod.rs` (replace stub)
- Create: `src/api/jobs.rs`
- Create: `src/api/queues.rs`
- Create: `src/api/health.rs`
- Test: `tests/api_jobs.rs` (integration test)

**Step 1: Write failing integration test**

```rust
// tests/api_jobs.rs
use reqwest::Client;
use serde_json::json;

/// Helper to start a test server and return its base URL
async fn start_test_server() -> (String, tokio::task::JoinHandle<()>) {
    // Bind to port 0 for random available port
    // Create storage in tempdir
    // Start axum server
    // Return (base_url, handle)
}

#[tokio::test]
async fn test_push_job_via_http() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base_url}/api/v1/queues/emails/jobs"))
        .json(&json!({
            "name": "send-welcome",
            "data": {"to": "user@example.com"}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["ok"].as_bool().unwrap());
    assert!(body["id"].as_str().is_some());
}

#[tokio::test]
async fn test_get_job_via_http() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    // Push first
    let push_resp = client
        .post(format!("{base_url}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "test", "data": {}}))
        .send().await.unwrap();
    let push_body: serde_json::Value = push_resp.json().await.unwrap();
    let job_id = push_body["id"].as_str().unwrap();

    // Get
    let get_resp = client
        .get(format!("{base_url}/api/v1/jobs/{job_id}"))
        .send().await.unwrap();
    assert_eq!(get_resp.status(), 200);
}

#[tokio::test]
async fn test_pull_ack_flow() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    // Push
    client.post(format!("{base_url}/api/v1/queues/q/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();

    // Pull
    let pull_resp = client
        .get(format!("{base_url}/api/v1/queues/q/jobs"))
        .send().await.unwrap();
    assert_eq!(pull_resp.status(), 200);
    let pull_body: serde_json::Value = pull_resp.json().await.unwrap();
    let job_id = pull_body["job"]["id"].as_str().unwrap();

    // Ack
    let ack_resp = client
        .post(format!("{base_url}/api/v1/jobs/{job_id}/ack"))
        .json(&json!({"result": {"sent": true}}))
        .send().await.unwrap();
    assert_eq!(ack_resp.status(), 200);
}

#[tokio::test]
async fn test_health_endpoint() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();
    let resp = client.get(format!("{base_url}/api/v1/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["ok"].as_bool().unwrap());
    assert_eq!(body["status"], "healthy");
}

#[tokio::test]
async fn test_list_queues() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    client.post(format!("{base_url}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();

    let resp = client.get(format!("{base_url}/api/v1/queues")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_queue_stats() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    client.post(format!("{base_url}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();

    let resp = client
        .get(format!("{base_url}/api/v1/queues/emails/stats"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["waiting"], 1);
}

#[tokio::test]
async fn test_fail_endpoint() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    client.post(format!("{base_url}/api/v1/queues/q/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();
    let pull_resp = client.get(format!("{base_url}/api/v1/queues/q/jobs"))
        .send().await.unwrap();
    let pull_body: serde_json::Value = pull_resp.json().await.unwrap();
    let job_id = pull_body["job"]["id"].as_str().unwrap();

    let resp = client
        .post(format!("{base_url}/api/v1/jobs/{job_id}/fail"))
        .json(&json!({"error": "timeout"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_cancel_endpoint() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    let push_resp = client.post(format!("{base_url}/api/v1/queues/q/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();
    let push_body: serde_json::Value = push_resp.json().await.unwrap();
    let job_id = push_body["id"].as_str().unwrap();

    let resp = client
        .post(format!("{base_url}/api/v1/jobs/{job_id}/cancel"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_batch_push() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    let resp = client.post(format!("{base_url}/api/v1/queues/q/jobs"))
        .json(&json!([
            {"name": "j1", "data": {}},
            {"name": "j2", "data": {}},
            {"name": "j3", "data": {}}
        ]))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ids"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_not_found_returns_404() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();
    let resp = client
        .get(format!("{base_url}/api/v1/jobs/00000000-0000-0000-0000-000000000000"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404);
}
```

**Step 2: Run tests — expect failures**

**Step 3: Implement HTTP API**

Build axum router with handlers that delegate to `QueueManager`:

```rust
// src/api/mod.rs
pub mod jobs;
pub mod queues;
pub mod health;

use axum::Router;
use std::sync::Arc;
use crate::engine::queue::QueueManager;

pub struct AppState {
    pub queue_manager: Arc<QueueManager>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(jobs::routes())
        .merge(queues::routes())
        .merge(health::routes())
        .with_state(state)
}
```

**Step 4: Run all tests**

Run: `cargo test -- -v`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add src/api/ tests/
git commit -m "feat(api): add HTTP REST API for jobs, queues, health"
```

---

## Task 8: TCP Protocol

**Files:**
- Create: `src/protocol/mod.rs` (replace stub)
- Create: `src/protocol/handler.rs`
- Test: `tests/tcp_protocol.rs`

**Step 1: Write failing integration test**

```rust
// tests/tcp_protocol.rs
use tokio::net::TcpStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use serde_json::json;

async fn connect_tcp(port: u16) -> (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf) {
    let stream = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
    let (reader, writer) = stream.into_split();
    (BufReader::new(reader), writer)
}

async fn send_cmd(writer: &mut tokio::net::tcp::OwnedWriteHalf, cmd: serde_json::Value) {
    let mut line = serde_json::to_string(&cmd).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
}

async fn read_response(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn test_tcp_push_and_pull() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Push
    send_cmd(&mut writer, json!({"cmd": "push", "queue": "q", "name": "j", "data": {}})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
    let job_id = resp["id"].as_str().unwrap().to_string();

    // Pull
    send_cmd(&mut writer, json!({"cmd": "pull", "queue": "q"})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
    assert_eq!(resp["job"]["id"].as_str().unwrap(), job_id);

    // Ack
    send_cmd(&mut writer, json!({"cmd": "ack", "id": job_id})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
}

#[tokio::test]
async fn test_tcp_invalid_command() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    send_cmd(&mut writer, json!({"cmd": "invalid"})).await;
    let resp = read_response(&mut reader).await;
    assert!(!resp["ok"].as_bool().unwrap());
    assert!(resp["error"]["code"].as_str().is_some());
}

#[tokio::test]
async fn test_tcp_malformed_json() {
    let port = start_test_tcp_server().await;
    let stream = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer.write_all(b"not json\n").await.unwrap();
    let resp = read_response(&mut reader).await;
    assert!(!resp["ok"].as_bool().unwrap());
}
```

**Step 2: Run tests — expect failures**

**Step 3: Implement TCP protocol handler**

```rust
// src/protocol/handler.rs
// Accept TCP connections, read lines, parse JSON commands,
// dispatch to QueueManager, write JSON responses
```

The TCP handler:
1. Listens on the configured TCP port
2. For each connection, spawns a tokio task
3. Reads newline-delimited JSON
4. Parses `cmd` field: `push`, `pull`, `ack`, `fail`, `cancel`, `stats`
5. Delegates to `QueueManager`
6. Writes JSON response + newline

**Step 4: Run tests**

Run: `cargo test tcp_protocol -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add src/protocol/ tests/tcp_protocol.rs
git commit -m "feat(protocol): add TCP newline-delimited JSON protocol"
```

---

## Task 9: Server Startup — Wire Everything Together

**Files:**
- Modify: `src/main.rs` — wire config → storage → engine → API + TCP
- Create: `src/server.rs` — server lifecycle management
- Test: `tests/server_smoke.rs`

**Step 1: Write failing test**

```rust
// tests/server_smoke.rs
#[tokio::test]
async fn test_server_starts_and_responds_to_health() {
    // Start full server with random ports
    // Hit health endpoint
    // Verify 200 OK
}

#[tokio::test]
async fn test_server_cli_help() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("rustqueue").unwrap();
    cmd.arg("--help");
    cmd.assert().success().stdout(predicates::str::contains("job scheduler"));
}
```

**Step 2: Run test — expect failure**

**Step 3: Wire server startup**

```rust
// In main.rs Commands::Serve handler:
// 1. Load config (file → env → CLI overrides)
// 2. Initialize storage (RedbStorage)
// 3. Create QueueManager with storage
// 4. Start HTTP server (axum) on http_port
// 5. Start TCP server on tcp_port
// 6. Wait for shutdown signal (Ctrl+C)
```

**Step 4: Run tests**

Run: `cargo test server_smoke -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add src/main.rs src/server.rs tests/server_smoke.rs
git commit -m "feat: wire server startup — config, storage, HTTP, TCP"
```

---

## Task 10: Property-Based Tests for Job State Machine

**Files:**
- Create: `tests/property_tests.rs`

**Step 1: Write property tests**

```rust
// tests/property_tests.rs
use proptest::prelude::*;
use rustqueue::*;

// Strategy to generate valid job states
fn arb_job_state() -> impl Strategy<Value = JobState> {
    prop_oneof![
        Just(JobState::Created),
        Just(JobState::Waiting),
        Just(JobState::Delayed),
        Just(JobState::Active),
        Just(JobState::Completed),
        Just(JobState::Failed),
        Just(JobState::Dlq),
        Just(JobState::Cancelled),
        Just(JobState::Blocked),
    ]
}

proptest! {
    #[test]
    fn job_serialization_roundtrip(
        queue in "[a-z]{1,20}",
        name in "[a-z-]{1,30}",
        priority in -100i32..100,
    ) {
        let mut job = Job::new(queue, name, serde_json::json!({}));
        job.priority = priority;
        let json = serde_json::to_string(&job).unwrap();
        let roundtripped: Job = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(job.id, roundtripped.id);
        prop_assert_eq!(job.priority, roundtripped.priority);
    }

    #[test]
    fn all_job_states_serialize(state in arb_job_state()) {
        let json = serde_json::to_string(&state).unwrap();
        let roundtripped: JobState = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(state, roundtripped);
    }

    #[test]
    fn backoff_delay_calculation(
        attempt in 1u32..20,
        base_delay in 100u64..10000,
    ) {
        // Exponential: delay * 2^attempt should not overflow for reasonable values
        let delay = base_delay.checked_mul(2u64.pow(attempt.min(16)));
        prop_assert!(delay.is_some() || attempt > 16);
    }
}
```

**Step 2: Run property tests**

Run: `cargo test property_tests -- -v`
Expected: PASS (100+ random cases each)

**Step 3: Commit**

```bash
git add tests/property_tests.rs
git commit -m "test: add property-based tests for job state machine"
```

---

## Task 11: Throughput Benchmarks

**Files:**
- Modify: `benches/throughput.rs` — real benchmarks

**Step 1: Implement benchmarks**

```rust
// benches/throughput.rs
use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use rustqueue::engine::models::Job;
use rustqueue::storage::StorageBackend;
// Use RedbStorage with tempdir

fn bench_push_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Create temp storage
    c.bench_function("push_single_job", |b| {
        b.to_async(&rt).iter(|| async {
            // Push one job
        })
    });
}

fn bench_push_pull_ack(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("push_pull_ack_roundtrip", |b| {
        b.to_async(&rt).iter(|| async {
            // Push, pull, ack one job
        })
    });
}

fn bench_batch_push(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("batch_push");
    for size in [10, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.to_async(&rt).iter(|| async move {
                // Push `size` jobs in a batch
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_push_throughput, bench_push_pull_ack, bench_batch_push);
criterion_main!(benches);
```

**Step 2: Run benchmarks**

Run: `cargo bench`
Expected: Numbers printed, establishing baseline

**Step 3: Commit**

```bash
git add benches/
git commit -m "bench: add throughput benchmarks for push, pull, ack"
```

---

## Task 12: Prometheus Metrics

**Files:**
- Create: `src/engine/metrics.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/api/mod.rs` — add metrics endpoint
- Test: `tests/api_jobs.rs` — add metrics test

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_prometheus_metrics_endpoint() {
    let (base_url, _handle) = start_test_server().await;
    let client = Client::new();

    // Push a job to generate some metrics
    client.post(format!("{base_url}/api/v1/queues/emails/jobs"))
        .json(&json!({"name": "j", "data": {}}))
        .send().await.unwrap();

    let resp = client.get(format!("{base_url}/api/v1/metrics/prometheus"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("rustqueue_jobs_pushed_total"));
}
```

**Step 2: Run test — expect failure**

**Step 3: Implement metrics**

Use `metrics` crate macros (`counter!`, `gauge!`, `histogram!`) in QueueManager operations. Expose via `metrics-exporter-prometheus` at the metrics endpoint.

**Step 4: Run tests**

Run: `cargo test prometheus -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add src/engine/metrics.rs src/engine/mod.rs src/api/
git commit -m "feat(metrics): add Prometheus metrics for jobs pushed/completed/failed"
```

---

## Task 13: Structured Logging

**Files:**
- Modify: `src/main.rs` — configure tracing-subscriber with JSON output
- Modify: `src/engine/queue.rs` — add tracing spans to operations
- Test: verify in integration tests that structured fields appear

**Step 1: Add tracing spans to QueueManager**

Add `#[tracing::instrument]` to key methods. Use structured fields:
```rust
#[tracing::instrument(skip(self, data), fields(queue, name))]
pub async fn push(&self, queue: &str, name: &str, data: Value, opts: Option<JobOptions>) -> Result<JobId> {
    // ...
    tracing::info!(job_id = %id, "Job pushed");
    Ok(id)
}
```

**Step 2: Configure JSON logging**

In `main.rs`, when `RUSTQUEUE_LOG_FORMAT=json`, use `tracing_subscriber::fmt().json()`.

**Step 3: Commit**

```bash
git add src/main.rs src/engine/queue.rs
git commit -m "feat(logging): add structured tracing with JSON output support"
```

---

## Task 14: Final Integration Test — Full Lifecycle

**Files:**
- Create: `tests/full_lifecycle.rs`

**Step 1: Write comprehensive lifecycle test**

```rust
// tests/full_lifecycle.rs
// Tests the complete job lifecycle end-to-end:
// 1. Start server
// 2. Push job via HTTP
// 3. Pull job via HTTP (or TCP)
// 4. Send progress update
// 5. Ack job
// 6. Verify completed state
// 7. Check queue stats show 0 waiting, 1 completed
// 8. Check health endpoint
// 9. Push job, pull, fail 3 times → verify DLQ
// 10. Push delayed job → verify not dequeued
// 11. Push with unique key → verify dedup

#[tokio::test]
async fn test_full_job_lifecycle() {
    // ... comprehensive test covering happy path and error paths
}
```

**Step 2: Run test**

Run: `cargo test full_lifecycle -- -v`
Expected: PASS

**Step 3: Commit**

```bash
git add tests/full_lifecycle.rs
git commit -m "test: add full lifecycle integration test"
```

---

## Summary — Phase 1 Test Coverage

| Component | Unit Tests | Integration Tests | Property Tests | Benchmarks |
|-----------|-----------|------------------|----------------|------------|
| `engine::models` | 4 (serialization, defaults) | — | 3 (roundtrip, states, backoff) | — |
| `storage::redb` | 12+ (CRUD, dequeue, DLQ, schedules, cleanup) | — | — | 3 (push, pull+ack, batch) |
| `engine::queue` | 10+ (push, pull, ack, fail, cancel, dedup, delay) | — | — | — |
| `engine::error` | 2 (codes, status) | — | — | — |
| `api` (HTTP) | — | 11 (all endpoints, errors) | — | — |
| `protocol` (TCP) | — | 3 (push+pull, invalid cmd, malformed) | — | — |
| `server` | — | 2 (health, CLI help) | — | — |
| `full_lifecycle` | — | 1 (comprehensive) | — | — |
| **Total** | **28+** | **17+** | **3** | **3** |

---

## Phase 1 Exit Criteria Checklist

- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy` — no warnings
- [ ] `cargo bench` — baseline numbers recorded
- [ ] Push 10,000 jobs/sec throughput achieved (benchmark)
- [ ] Zero data loss on kill -9 (integration test with redb)
- [ ] HTTP API matches PRD section 11.2 endpoints
- [ ] TCP protocol matches PRD section 11.3 format
- [ ] `rustqueue serve` starts and responds to health check
- [ ] Error responses match PRD section 11.4 format
- [ ] Prometheus metrics exposed at `/api/v1/metrics/prometheus`

---

## Dependency Graph

```
Task 1 (Config) ──────────────────────────────┐
                                                │
Task 2 (Storage: insert/get) ──┐               │
                                │               │
Task 3 (Storage: update/dequeue)│               │
                                │               │
Task 4 (Storage: counts/DLQ)───┤               │
                                │               │
Task 6 (Error types) ──────────┤               │
                                ▼               ▼
                Task 5 (Queue Manager) ─────────┤
                        │                       │
                        ├───► Task 7 (HTTP API) │
                        │                       │
                        ├───► Task 8 (TCP)      │
                        │                       │
                        └───► Task 9 (Server)◄──┘
                                │
                Task 10 (Property tests)
                Task 11 (Benchmarks)
                Task 12 (Metrics)
                Task 13 (Logging)
                Task 14 (Full lifecycle test)
```

Tasks 1-4 and 6 can be parallelized. Task 5 depends on 2-4 and 6. Tasks 7-8 depend on 5. Task 9 depends on 1, 7, 8. Tasks 10-14 can run after 9.
