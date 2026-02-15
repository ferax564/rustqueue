//! Core queue manager — high-level business logic for job lifecycle operations.
//!
//! [`QueueManager`] wraps an `Arc<dyn StorageBackend>` and exposes push, pull,
//! ack, fail, cancel, and query operations with proper state-machine validation.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};

use tracing::{info, warn};

use crate::api::websocket::JobEvent;
use crate::engine::error::RustQueueError;
use crate::engine::metrics as metric_names;
use crate::engine::models::{BackoffStrategy, Job, JobId, JobState, QueueCounts, Schedule};
use crate::engine::plugins::WorkerRegistry;
use crate::engine::rate_limit::QueueRateLimiter;
use crate::storage::{CompleteJobOutcome, StorageBackend};

// ── Validation constants ─────────────────────────────────────────────────────

/// Maximum length of a queue name in bytes.
pub const MAX_QUEUE_NAME_LEN: usize = 256;
/// Maximum length of a job name in bytes.
pub const MAX_JOB_NAME_LEN: usize = 1024;
/// Maximum size of job data payload in bytes (1 MB).
pub const MAX_JOB_DATA_SIZE: usize = 1_048_576;
/// Maximum length of a unique key in bytes.
pub const MAX_UNIQUE_KEY_LEN: usize = 1024;
/// Maximum length of an error message in bytes (10 KB).
pub const MAX_ERROR_MESSAGE_LEN: usize = 10_240;
/// Maximum size of job metadata in bytes (64 KB).
pub const MAX_METADATA_SIZE: usize = 65_536;
/// Reserved metadata key used for automatic cross-queue follow-up jobs.
const FOLLOW_UPS_METADATA_KEY: &str = "follow_ups";

/// Estimate the JSON-serialized byte size of a `serde_json::Value` without allocating.
///
/// The estimate slightly overcounts (counts a separator after every element, including
/// the last), making it a safe upper bound for rejecting oversized payloads. This avoids
/// the cost of `serde_json::to_string` (allocation + serialization) on every push.
fn estimate_json_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(true) => 4,
        serde_json::Value::Bool(false) => 5,
        serde_json::Value::Number(n) => n.to_string().len(),
        serde_json::Value::String(s) => s.len() + 2, // quotes
        serde_json::Value::Array(arr) => {
            // [] + elements with commas
            2 + arr.iter().map(|v| estimate_json_size(v) + 1).sum::<usize>()
        }
        serde_json::Value::Object(map) => {
            // {} + "key":value, for each entry
            2 + map
                .iter()
                .map(|(k, v)| k.len() + 3 + estimate_json_size(v) + 1)
                .sum::<usize>()
        }
    }
}

/// Validate that a name contains only safe characters: alphanumeric, dash, underscore, dot.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Validate input for a push operation. Returns a `ValidationError` on failure.
fn validate_push_input(
    queue: &str,
    name: &str,
    data: &serde_json::Value,
    opts: &Option<JobOptions>,
) -> Result<(), RustQueueError> {
    // Queue name
    if queue.len() > MAX_QUEUE_NAME_LEN {
        return Err(RustQueueError::ValidationError(format!(
            "Queue name exceeds maximum length of {MAX_QUEUE_NAME_LEN} bytes"
        )));
    }
    if !is_valid_name(queue) {
        return Err(RustQueueError::ValidationError(
            "Queue name must be non-empty and contain only alphanumeric characters, dashes, underscores, or dots".into()
        ));
    }

    // Job name
    if name.len() > MAX_JOB_NAME_LEN {
        return Err(RustQueueError::ValidationError(format!(
            "Job name exceeds maximum length of {MAX_JOB_NAME_LEN} bytes"
        )));
    }
    if !is_valid_name(name) {
        return Err(RustQueueError::ValidationError(
            "Job name must be non-empty and contain only alphanumeric characters, dashes, underscores, or dots".into()
        ));
    }

    // Data payload size — use fast estimate to avoid serializing the entire payload.
    // The estimate overcounts slightly, so only do the exact check if it exceeds the limit.
    let estimated_size = estimate_json_size(data);
    if estimated_size > MAX_JOB_DATA_SIZE {
        // Near the limit — do the exact check
        let data_size = serde_json::to_string(data).map(|s| s.len()).unwrap_or(0);
        if data_size > MAX_JOB_DATA_SIZE {
            return Err(RustQueueError::ValidationError(format!(
                "Job data payload ({data_size} bytes) exceeds maximum of {MAX_JOB_DATA_SIZE} bytes"
            )));
        }
    }

    // Unique key and metadata
    if let Some(opts) = opts {
        if let Some(uk) = &opts.unique_key {
            if uk.len() > MAX_UNIQUE_KEY_LEN {
                return Err(RustQueueError::ValidationError(format!(
                    "Unique key exceeds maximum length of {MAX_UNIQUE_KEY_LEN} bytes"
                )));
            }
        }
        if let Some(ref meta) = opts.metadata {
            let estimated_size = estimate_json_size(meta);
            if estimated_size > MAX_METADATA_SIZE {
                let meta_size = serde_json::to_string(meta).map(|s| s.len()).unwrap_or(0);
                if meta_size > MAX_METADATA_SIZE {
                    return Err(RustQueueError::ValidationError(format!(
                        "Job metadata ({meta_size} bytes) exceeds maximum of {MAX_METADATA_SIZE} bytes"
                    )));
                }
            }
        }
    }

    Ok(())
}

// ── Public types ─────────────────────────────────────────────────────────────

/// Options that can be supplied when pushing a new job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
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
    pub custom_id: Option<String>,
    /// Arbitrary orchestration metadata (separate from job data payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Parent job IDs that must complete before this job becomes Waiting.
    #[schema(value_type = Option<Vec<String>>)]
    pub depends_on: Option<Vec<JobId>>,
    /// Flow identifier grouping related jobs in a DAG.
    pub flow_id: Option<String>,
}

/// Metadata-driven follow-up job descriptor for cross-engine chaining.
///
/// Stored under `metadata.follow_ups` on a parent job.
#[derive(Debug, Clone, Deserialize)]
struct FollowUpJobSpec {
    pub queue: String,
    pub name: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<JobOptions>,
    #[serde(default)]
    pub flow_id: Option<String>,
}

/// Result returned by [`QueueManager::fail`] to indicate retry disposition.
#[derive(Debug)]
pub struct FailResult {
    pub will_retry: bool,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// High-level information about a single queue.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueueInfo {
    pub name: String,
    pub counts: QueueCounts,
}

/// Single item in a batch push operation.
#[derive(Debug, Clone)]
pub struct BatchPushItem {
    pub name: String,
    pub data: serde_json::Value,
    pub options: Option<JobOptions>,
}

/// Single item in a batch acknowledgement operation.
#[derive(Debug, Clone)]
pub struct BatchAckItem {
    pub id: JobId,
    pub result: Option<serde_json::Value>,
}

/// Result for one item in a batch acknowledgement operation.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BatchAckResult {
    #[schema(value_type = String, format = "uuid")]
    pub id: JobId,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

// ── QueueManager ─────────────────────────────────────────────────────────────

/// Core engine that mediates between callers and the storage backend.
pub struct QueueManager {
    storage: Arc<dyn StorageBackend>,
    event_tx: Option<tokio::sync::broadcast::Sender<JobEvent>>,
    /// Set of paused queue names. Pushes to paused queues are rejected.
    paused_queues: DashSet<String>,
    /// Reverse dependency index: parent_id → list of child job IDs.
    /// Used to resolve Blocked → Waiting when a parent completes.
    dependency_index: DashMap<JobId, Vec<JobId>>,
    /// Maximum depth for DAG dependency chains (cycle detection).
    max_dag_depth: usize,
    /// Optional registry for pluggable workers, keyed by engine.
    worker_registry: Option<Arc<WorkerRegistry>>,
    /// Optional per-queue token-bucket rate limiter.
    rate_limiter: Option<Arc<QueueRateLimiter>>,
}

impl QueueManager {
    /// Create a new `QueueManager` backed by the given storage implementation.
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self {
        Self {
            storage,
            event_tx: None,
            paused_queues: DashSet::new(),
            dependency_index: DashMap::new(),
            max_dag_depth: 10,
            worker_registry: None,
            rate_limiter: None,
        }
    }

    /// Attach a broadcast sender for emitting real-time job events.
    ///
    /// Must be called **before** wrapping the manager in `Arc`.
    pub fn with_event_sender(mut self, tx: tokio::sync::broadcast::Sender<JobEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Set the maximum DAG depth for dependency chains.
    ///
    /// Must be called **before** wrapping the manager in `Arc`.
    pub fn with_max_dag_depth(mut self, depth: usize) -> Self {
        self.max_dag_depth = depth;
        self
    }

    /// Attach a pluggable worker registry for cross-engine dispatch.
    ///
    /// Must be called **before** wrapping the manager in `Arc`.
    pub fn with_worker_registry(mut self, registry: Arc<WorkerRegistry>) -> Self {
        self.worker_registry = Some(registry);
        self
    }

    /// Attach a per-queue rate limiter.
    ///
    /// Must be called **before** wrapping the manager in `Arc`.
    pub fn with_rate_limiter(mut self, limiter: Arc<QueueRateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Emit a job event to all connected WebSocket clients.
    ///
    /// Silently ignores failures (no connected receivers is fine).
    fn emit_event(&self, event: &str, job_id: JobId, queue: &str) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(JobEvent {
                event: event.to_string(),
                job_id,
                queue: queue.to_string(),
                timestamp: Utc::now(),
            });
        }
    }

    /// Apply optional push options to a job before it is persisted.
    fn apply_job_options(job: &mut Job, opts: Option<JobOptions>) {
        if let Some(opts) = opts {
            if let Some(p) = opts.priority {
                job.priority = p;
            }
            if let Some(ma) = opts.max_attempts {
                job.max_attempts = ma;
            }
            if let Some(b) = opts.backoff {
                job.backoff = b;
            }
            if let Some(bd) = opts.backoff_delay_ms {
                job.backoff_delay_ms = bd;
            }
            if let Some(ttl) = opts.ttl_ms {
                job.ttl_ms = Some(ttl);
            }
            if let Some(timeout) = opts.timeout_ms {
                job.timeout_ms = Some(timeout);
            }
            if let Some(ref uk) = opts.unique_key {
                job.unique_key = Some(uk.clone());
            }
            if let Some(tags) = opts.tags {
                job.tags = tags;
            }
            if let Some(gid) = opts.group_id {
                job.group_id = Some(gid);
            }
            if let Some(lifo) = opts.lifo {
                job.lifo = lifo;
            }
            if let Some(roc) = opts.remove_on_complete {
                job.remove_on_complete = roc;
            }
            if let Some(rof) = opts.remove_on_fail {
                job.remove_on_fail = rof;
            }
            if let Some(cid) = opts.custom_id {
                job.custom_id = Some(cid);
            }
            if let Some(meta) = opts.metadata {
                job.metadata = Some(meta);
            }
            if let Some(deps) = opts.depends_on {
                if !deps.is_empty() {
                    job.depends_on = deps;
                }
            }
            if let Some(fid) = opts.flow_id {
                job.flow_id = Some(fid);
            }

            // Delay handling — must come after unique_key is applied.
            if let Some(delay_ms) = opts.delay_ms {
                job.delay_until = Some(Utc::now() + Duration::milliseconds(delay_ms as i64));
                job.state = JobState::Delayed;
            }
        }
    }

    // ── Push ─────────────────────────────────────────────────────────────

    /// Enqueue a new job, applying the supplied [`JobOptions`].
    ///
    /// Returns the generated [`JobId`].
    #[tracing::instrument(skip(self, data, opts), fields(queue, name))]
    pub async fn push(
        &self,
        queue: &str,
        name: &str,
        data: serde_json::Value,
        opts: Option<JobOptions>,
    ) -> Result<JobId, RustQueueError> {
        let start = std::time::Instant::now();

        // Input validation
        validate_push_input(queue, name, &data, &opts)?;

        // Check if queue is paused
        if self.paused_queues.contains(queue) {
            return Err(RustQueueError::QueuePaused(queue.to_string()));
        }

        // Per-queue rate limiting
        if let Some(ref rl) = self.rate_limiter {
            if !rl.check(queue) {
                metrics::counter!(metric_names::RATE_LIMIT_REJECTED_TOTAL, "queue" => queue.to_string()).increment(1);
                return Err(RustQueueError::RateLimited);
            }
        }

        let mut job = Job::new(queue, name, data);
        Self::apply_job_options(&mut job, opts);

        // Unique-key deduplication check.
        if let Some(ref uk) = job.unique_key {
            let existing = self
                .storage
                .get_job_by_unique_key(queue, uk)
                .await
                .map_err(RustQueueError::Internal)?;
            if existing.is_some() {
                return Err(RustQueueError::DuplicateKey(uk.clone()));
            }
        }

        // ── DAG dependency handling ─────────────────────────────────────
        if !job.depends_on.is_empty() {
            // Validate all parent jobs exist and aren't terminally failed
            for &parent_id in &job.depends_on {
                let parent = self
                    .storage
                    .get_job(parent_id)
                    .await
                    .map_err(RustQueueError::Internal)?
                    .ok_or_else(|| {
                        RustQueueError::ValidationError(format!(
                            "Dependency parent job '{parent_id}' not found"
                        ))
                    })?;
                if matches!(parent.state, JobState::Dlq | JobState::Cancelled) {
                    return Err(RustQueueError::ValidationError(format!(
                        "Dependency parent job '{parent_id}' is in terminal state '{:?}'",
                        parent.state
                    )));
                }
            }

            // Cycle detection via BFS
            self.detect_cycle(&job).await?;

            // Determine initial state: if all deps already completed, go to Waiting;
            // otherwise start as Blocked.
            let all_completed = self.all_deps_completed(&job.depends_on).await?;
            if !all_completed {
                job.state = JobState::Blocked;
            }
        }

        let id = self
            .storage
            .insert_job(&job)
            .await
            .map_err(RustQueueError::Internal)?;

        // Populate reverse dependency index
        for &parent_id in &job.depends_on {
            self.dependency_index.entry(parent_id).or_default().push(id);
        }

        metrics::counter!(metric_names::JOBS_PUSHED_TOTAL).increment(1);
        metrics::histogram!(metric_names::PUSH_DURATION_SECONDS)
            .record(start.elapsed().as_secs_f64());

        self.emit_event("job.pushed", id, queue);

        info!(job_id = %id, "Job pushed");

        Ok(id)
    }

    /// Enqueue multiple jobs and persist them via the backend batch API.
    ///
    /// Jobs are validated and prepared first, then inserted through a single
    /// storage call. Backends may implement this as a single transaction.
    #[tracing::instrument(skip(self, items), fields(queue, batch_size = items.len()))]
    pub async fn push_batch(
        &self,
        queue: &str,
        items: Vec<BatchPushItem>,
    ) -> Result<Vec<JobId>, RustQueueError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        // Check if queue is paused
        if self.paused_queues.contains(queue) {
            return Err(RustQueueError::QueuePaused(queue.to_string()));
        }

        // Per-queue rate limiting (check once for the whole batch)
        if let Some(ref rl) = self.rate_limiter {
            if !rl.check(queue) {
                metrics::counter!(metric_names::RATE_LIMIT_REJECTED_TOTAL, "queue" => queue.to_string()).increment(1);
                return Err(RustQueueError::RateLimited);
            }
        }

        let mut jobs = Vec::with_capacity(items.len());
        let mut unique_keys_in_batch = HashSet::new();

        for item in items {
            // Validate each item in the batch
            validate_push_input(queue, &item.name, &item.data, &item.options)?;

            let mut job = Job::new(queue, &item.name, item.data);
            Self::apply_job_options(&mut job, item.options);

            if let Some(ref uk) = job.unique_key {
                if !unique_keys_in_batch.insert(uk.clone()) {
                    return Err(RustQueueError::DuplicateKey(uk.clone()));
                }

                let existing = self
                    .storage
                    .get_job_by_unique_key(queue, uk)
                    .await
                    .map_err(RustQueueError::Internal)?;
                if existing.is_some() {
                    return Err(RustQueueError::DuplicateKey(uk.clone()));
                }
            }

            jobs.push(job);
        }

        let ids = self
            .storage
            .insert_jobs_batch(&jobs)
            .await
            .map_err(RustQueueError::Internal)?;

        metrics::counter!(metric_names::JOBS_PUSHED_TOTAL).increment(ids.len() as u64);

        for id in &ids {
            self.emit_event("job.pushed", *id, queue);
        }

        info!(queue, count = ids.len(), "Batch jobs pushed");

        Ok(ids)
    }

    // ── Pull ─────────────────────────────────────────────────────────────

    /// Dequeue up to `count` jobs from the given queue.
    ///
    /// Returned jobs are transitioned to [`JobState::Active`] atomically.
    #[tracing::instrument(skip(self), fields(queue, count))]
    pub async fn pull(&self, queue: &str, count: u32) -> Result<Vec<Job>, RustQueueError> {
        let start = std::time::Instant::now();

        let jobs = self
            .storage
            .dequeue(queue, count)
            .await
            .map_err(RustQueueError::Internal)?;

        if !jobs.is_empty() {
            metrics::counter!(metric_names::JOBS_PULLED_TOTAL).increment(jobs.len() as u64);
        }

        metrics::histogram!(metric_names::PULL_DURATION_SECONDS)
            .record(start.elapsed().as_secs_f64());

        Ok(jobs)
    }

    /// Pull one job from a queue and dispatch it to a registered engine worker.
    ///
    /// The worker is resolved from `metadata.engine` first, then queue routing
    /// rules in the attached [`WorkerRegistry`]. On success, the job is acked.
    /// On processor error, the job is failed through normal retry/DLQ logic.
    pub async fn dispatch_next_with_registered_worker(
        &self,
        queue: &str,
    ) -> Result<Option<JobId>, RustQueueError> {
        let registry = self.worker_registry.as_ref().ok_or_else(|| {
            RustQueueError::ValidationError("Worker registry is not configured".to_string())
        })?;

        let mut jobs = self.pull(queue, 1).await?;
        let Some(job) = jobs.pop() else {
            return Ok(None);
        };

        let worker = registry
            .resolve_worker(&job.queue, job.metadata.as_ref())?
            .ok_or_else(|| {
                RustQueueError::ValidationError(format!(
                    "No worker route matched queue '{}' (metadata.engine or queue route required)",
                    job.queue
                ))
            })?;

        let job_id = job.id;
        match worker.process(job).await {
            Ok(result) => {
                self.ack(job_id, result).await?;
                Ok(Some(job_id))
            }
            Err(process_err) => {
                let reason = process_err.to_string();
                if let Err(fail_err) = self.fail(job_id, &reason).await {
                    warn!(
                        job_id = %job_id,
                        process_error = %process_err,
                        fail_error = %fail_err,
                        "worker processing failed and fail transition also failed"
                    );
                    return Err(fail_err);
                }
                Err(process_err)
            }
        }
    }

    // ── Ack ──────────────────────────────────────────────────────────────

    /// Acknowledge successful completion of a job.
    ///
    /// The job must be in [`JobState::Active`]; otherwise an error is returned.
    #[tracing::instrument(skip(self, result), fields(id = %id))]
    pub async fn ack(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<(), RustQueueError> {
        let start = std::time::Instant::now();

        let job = match self
            .storage
            .complete_job(id, result)
            .await
            .map_err(RustQueueError::Internal)?
        {
            CompleteJobOutcome::Completed(job) => job,
            CompleteJobOutcome::NotFound => {
                return Err(RustQueueError::JobNotFound(id.to_string()));
            }
            CompleteJobOutcome::InvalidState(current) => {
                return Err(RustQueueError::InvalidState {
                    current: format!("{:?}", current),
                    expected: "Active".to_string(),
                });
            }
        };

        metrics::counter!(metric_names::JOBS_COMPLETED_TOTAL).increment(1);
        metrics::histogram!(metric_names::ACK_DURATION_SECONDS)
            .record(start.elapsed().as_secs_f64());

        self.emit_event("job.completed", id, &job.queue);

        // Resolve dependencies: promote Blocked children whose deps are all complete.
        self.resolve_dependencies(id).await;
        // Fan-out cross-engine follow-up jobs, if configured in metadata.
        self.enqueue_follow_ups(&job).await;

        info!(job_id = %id, "Job acknowledged");

        Ok(())
    }

    /// Acknowledge completion for multiple jobs in one backend call.
    ///
    /// Returns one result per input item. A failure for one job does not
    /// prevent other jobs in the batch from being processed.
    #[tracing::instrument(skip(self, items), fields(batch_size = items.len()))]
    pub async fn ack_batch(
        &self,
        items: Vec<BatchAckItem>,
    ) -> Result<Vec<BatchAckResult>, RustQueueError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let payload: Vec<(JobId, Option<serde_json::Value>)> = items
            .iter()
            .map(|item| (item.id, item.result.clone()))
            .collect();

        let outcomes = self
            .storage
            .complete_jobs_batch(&payload)
            .await
            .map_err(RustQueueError::Internal)?;

        let mut completed = 0u64;
        let mut results = Vec::with_capacity(items.len());

        for (item, outcome) in items.into_iter().zip(outcomes.into_iter()) {
            match outcome {
                CompleteJobOutcome::Completed(job) => {
                    completed += 1;
                    self.emit_event("job.completed", item.id, &job.queue);
                    self.resolve_dependencies(item.id).await;
                    self.enqueue_follow_ups(&job).await;
                    results.push(BatchAckResult {
                        id: item.id,
                        ok: true,
                        error_code: None,
                        error_message: None,
                    });
                }
                CompleteJobOutcome::NotFound => {
                    results.push(BatchAckResult {
                        id: item.id,
                        ok: false,
                        error_code: Some("JOB_NOT_FOUND".to_string()),
                        error_message: Some(format!("Job '{}' not found", item.id)),
                    });
                }
                CompleteJobOutcome::InvalidState(current) => {
                    results.push(BatchAckResult {
                        id: item.id,
                        ok: false,
                        error_code: Some("INVALID_STATE".to_string()),
                        error_message: Some(format!(
                            "Job is in invalid state '{current:?}' for operation (expected: Active)"
                        )),
                    });
                }
            }
        }

        if completed > 0 {
            metrics::counter!(metric_names::JOBS_COMPLETED_TOTAL).increment(completed);
        }

        info!(
            acked = completed,
            failed = results.iter().filter(|r| !r.ok).count(),
            "Batch jobs acknowledged"
        );

        Ok(results)
    }

    // ── Fail ─────────────────────────────────────────────────────────────

    /// Report a job failure. The engine decides whether to retry or move to DLQ.
    #[tracing::instrument(skip(self), fields(id = %id, error))]
    pub async fn fail(&self, id: JobId, error: &str) -> Result<FailResult, RustQueueError> {
        if error.len() > MAX_ERROR_MESSAGE_LEN {
            return Err(RustQueueError::ValidationError(format!(
                "Error message ({} bytes) exceeds maximum of {MAX_ERROR_MESSAGE_LEN} bytes",
                error.len()
            )));
        }
        let mut job = self.require_job(id).await?;

        if job.state != JobState::Active {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Active".to_string(),
            });
        }

        job.attempt += 1;
        job.last_error = Some(error.to_string());
        job.updated_at = Utc::now();

        metrics::counter!(metric_names::JOBS_FAILED_TOTAL).increment(1);

        if job.attempt < job.max_attempts {
            // Retry: compute backoff delay and move back to Waiting (or Delayed).
            let delay_ms = Self::compute_backoff(job.backoff, job.backoff_delay_ms, job.attempt);
            let next = Utc::now() + Duration::milliseconds(delay_ms as i64);

            if delay_ms > 0 {
                job.state = JobState::Delayed;
                job.delay_until = Some(next);
            } else {
                job.state = JobState::Waiting;
            }

            self.storage
                .update_job(&job)
                .await
                .map_err(RustQueueError::Internal)?;

            info!(job_id = %id, attempt = job.attempt, "Job failed, will retry");

            Ok(FailResult {
                will_retry: true,
                next_attempt_at: if delay_ms > 0 { Some(next) } else { None },
            })
        } else {
            // Exhausted retries — move to DLQ.
            self.storage
                .move_to_dlq(&job, error)
                .await
                .map_err(RustQueueError::Internal)?;

            self.emit_event("job.failed", id, &job.queue);

            // Cascade failure: move all Blocked children to DLQ recursively.
            self.cascade_dependency_failure(id).await;

            info!(job_id = %id, "Job moved to DLQ");

            Ok(FailResult {
                will_retry: false,
                next_attempt_at: None,
            })
        }
    }

    // ── Cancel ───────────────────────────────────────────────────────────

    /// Cancel a job that is still waiting or delayed.
    ///
    /// Active, Completed, Failed, Dlq, and Cancelled jobs cannot be cancelled.
    #[tracing::instrument(skip(self), fields(id = %id))]
    pub async fn cancel(&self, id: JobId) -> Result<(), RustQueueError> {
        let mut job = self.require_job(id).await?;

        if !matches!(
            job.state,
            JobState::Waiting | JobState::Delayed | JobState::Blocked
        ) {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Waiting, Delayed, or Blocked".to_string(),
            });
        }

        job.state = JobState::Cancelled;
        job.updated_at = Utc::now();

        self.storage
            .update_job(&job)
            .await
            .map_err(RustQueueError::Internal)?;

        self.emit_event("job.cancelled", id, &job.queue);

        Ok(())
    }

    // ── Progress ─────────────────────────────────────────────────────────

    /// Update progress on an active job, optionally appending a log message.
    ///
    /// Progress is clamped to 0–100. The job must be in [`JobState::Active`].
    #[tracing::instrument(skip(self, message), fields(id = %id, progress))]
    pub async fn update_progress(
        &self,
        id: JobId,
        progress: u8,
        message: Option<String>,
    ) -> Result<(), RustQueueError> {
        let mut job = self.require_job(id).await?;

        if job.state != JobState::Active {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Active".to_string(),
            });
        }

        job.progress = Some(progress.min(100));
        job.updated_at = Utc::now();

        if let Some(msg) = message {
            job.logs.push(crate::engine::models::LogEntry {
                timestamp: Utc::now(),
                message: msg,
            });
        }

        self.storage
            .update_job(&job)
            .await
            .map_err(RustQueueError::Internal)?;

        Ok(())
    }

    // ── Get ──────────────────────────────────────────────────────────────

    /// Retrieve a single job by ID, or `None` if it does not exist.
    pub async fn get_job(&self, id: JobId) -> Result<Option<Job>, RustQueueError> {
        self.storage
            .get_job(id)
            .await
            .map_err(RustQueueError::Internal)
    }

    // ── Queue listing ────────────────────────────────────────────────────

    /// List all known queues together with their current counts.
    pub async fn list_queues(&self) -> Result<Vec<QueueInfo>, RustQueueError> {
        let names = self
            .storage
            .list_queue_names()
            .await
            .map_err(RustQueueError::Internal)?;

        let mut infos = Vec::with_capacity(names.len());
        for name in names {
            let counts = self
                .storage
                .get_queue_counts(&name)
                .await
                .map_err(RustQueueError::Internal)?;
            infos.push(QueueInfo { name, counts });
        }
        Ok(infos)
    }

    /// Get counts for a specific queue.
    pub async fn get_queue_stats(&self, queue: &str) -> Result<QueueCounts, RustQueueError> {
        self.storage
            .get_queue_counts(queue)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Retrieve dead-letter-queue jobs for a specific queue, up to `limit`.
    pub async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>, RustQueueError> {
        self.storage
            .get_dlq_jobs(queue, limit)
            .await
            .map_err(RustQueueError::Internal)
    }

    // ── Heartbeat ─────────────────────────────────────────────────────────

    /// Update the heartbeat timestamp for an active job.
    ///
    /// Workers should call this periodically to indicate they are still processing.
    #[tracing::instrument(skip(self), fields(id = %id))]
    pub async fn heartbeat(&self, id: JobId) -> Result<(), RustQueueError> {
        let mut job = self.require_job(id).await?;
        if job.state != JobState::Active {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Active".to_string(),
            });
        }
        job.last_heartbeat = Some(Utc::now());
        job.updated_at = Utc::now();
        self.storage
            .update_job(&job)
            .await
            .map_err(RustQueueError::Internal)?;
        Ok(())
    }

    // ── Stall detection ──────────────────────────────────────────────────

    /// Detect and fail stalled jobs — jobs that haven't sent a heartbeat within the timeout.
    ///
    /// A job is considered stalled if:
    /// `now - max(last_heartbeat, started_at) > stall_timeout`
    ///
    /// Returns the number of stalled jobs detected and failed.
    #[tracing::instrument(skip(self))]
    pub async fn detect_stalls(&self, stall_timeout_ms: u64) -> Result<u32, RustQueueError> {
        let active_jobs = self
            .storage
            .get_active_jobs()
            .await
            .map_err(RustQueueError::Internal)?;

        let now = Utc::now();
        let mut stalled = 0u32;

        for job in active_jobs {
            let last_alive = job.last_heartbeat.or(job.started_at);
            if let Some(last) = last_alive {
                let elapsed = (now - last).num_milliseconds();
                if elapsed > stall_timeout_ms as i64 {
                    match self.fail(job.id, "job stalled (no heartbeat)").await {
                        Ok(_) => stalled += 1,
                        Err(e) => {
                            tracing::warn!(job_id = %job.id, error = %e, "Failed to mark stalled job");
                        }
                    }
                }
            }
        }

        if stalled > 0 {
            tracing::info!(count = stalled, "Stalled jobs detected");
        }

        Ok(stalled)
    }

    // ── Delayed job promotion ─────────────────────────────────────────────

    /// Promote delayed jobs whose delay has expired to Waiting state.
    ///
    /// Returns the number of promoted jobs.
    #[tracing::instrument(skip(self))]
    pub async fn promote_delayed_jobs(&self) -> Result<u32, RustQueueError> {
        let now = Utc::now();
        let ready = self
            .storage
            .get_ready_scheduled(now)
            .await
            .map_err(RustQueueError::Internal)?;

        let mut promoted = 0u32;

        for mut job in ready {
            job.state = JobState::Waiting;
            job.delay_until = None;
            job.updated_at = Utc::now();

            if let Err(e) = self.storage.update_job(&job).await {
                tracing::warn!(job_id = %job.id, error = %e, "Failed to promote delayed job");
            } else {
                promoted += 1;
            }
        }

        if promoted > 0 {
            tracing::info!(count = promoted, "Promoted delayed jobs");
        }

        Ok(promoted)
    }

    // ── Timeout detection ─────────────────────────────────────────────────

    /// Check for active jobs that have exceeded their timeout and fail them.
    ///
    /// Returns the number of jobs that were timed out.
    #[tracing::instrument(skip(self))]
    pub async fn check_timeouts(&self) -> Result<u32, RustQueueError> {
        let active_jobs = self
            .storage
            .get_active_jobs()
            .await
            .map_err(RustQueueError::Internal)?;

        let now = Utc::now();
        let mut timed_out = 0u32;

        for job in active_jobs {
            if let Some(timeout_ms) = job.timeout_ms {
                if let Some(started_at) = job.started_at {
                    let elapsed = (now - started_at).num_milliseconds();
                    if elapsed > timeout_ms as i64 {
                        match self.fail(job.id, "job timed out").await {
                            Ok(_) => timed_out += 1,
                            Err(e) => {
                                tracing::warn!(job_id = %job.id, error = %e, "Failed to timeout job");
                            }
                        }
                    }
                }
            }
        }

        if timed_out > 0 {
            tracing::info!(count = timed_out, "Timed out jobs");
        }

        Ok(timed_out)
    }

    // ── Retention cleanup ──────────────────────────────────────────────

    /// Remove expired completed, failed, and DLQ jobs based on TTL strings.
    ///
    /// TTL strings use human-readable format: "7d", "24h", "30m".
    /// Returns a tuple of `(completed_removed, failed_removed, dlq_removed)`.
    pub async fn cleanup_expired_jobs(
        &self,
        completed_ttl: &str,
        failed_ttl: &str,
        dlq_ttl: &str,
    ) -> Result<(u64, u64, u64), RustQueueError> {
        let now = Utc::now();

        let completed = if let Some(dur) = parse_ttl(completed_ttl) {
            self.storage
                .remove_completed_before(now - dur)
                .await
                .map_err(RustQueueError::Internal)?
        } else {
            0
        };

        let failed = if let Some(dur) = parse_ttl(failed_ttl) {
            self.storage
                .remove_failed_before(now - dur)
                .await
                .map_err(RustQueueError::Internal)?
        } else {
            0
        };

        let dlq = if let Some(dur) = parse_ttl(dlq_ttl) {
            self.storage
                .remove_dlq_before(now - dur)
                .await
                .map_err(RustQueueError::Internal)?
        } else {
            0
        };

        if completed > 0 || failed > 0 || dlq > 0 {
            tracing::info!(completed, failed, dlq, "Retention cleanup");
        }

        Ok((completed, failed, dlq))
    }

    // ── Schedule management ──────────────────────────────────────────────

    /// Create or update a schedule. Validates that exactly one of cron_expr/every_ms is set.
    pub async fn create_schedule(&self, schedule: &Schedule) -> Result<(), RustQueueError> {
        // Validate: must have cron_expr or every_ms (but not both)
        if schedule.cron_expr.is_none() && schedule.every_ms.is_none() {
            return Err(RustQueueError::ValidationError(
                "Schedule must have cron_expr or every_ms".into(),
            ));
        }
        if schedule.cron_expr.is_some() && schedule.every_ms.is_some() {
            return Err(RustQueueError::ValidationError(
                "Schedule cannot have both cron_expr and every_ms".into(),
            ));
        }
        // Validate cron expression if present
        if let Some(ref cron) = schedule.cron_expr {
            croner::Cron::new(cron).parse().map_err(|e| {
                RustQueueError::ValidationError(format!("Invalid cron expression: {e}"))
            })?;
        }
        self.storage
            .upsert_schedule(schedule)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Retrieve a schedule by name, or `None` if it does not exist.
    pub async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>, RustQueueError> {
        self.storage
            .get_schedule(name)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// List all schedules (active and paused).
    pub async fn list_schedules(&self) -> Result<Vec<Schedule>, RustQueueError> {
        self.storage
            .list_all_schedules()
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Delete a schedule by name.
    pub async fn delete_schedule(&self, name: &str) -> Result<(), RustQueueError> {
        self.storage
            .delete_schedule(name)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Pause a schedule, preventing it from creating new jobs.
    pub async fn pause_schedule(&self, name: &str) -> Result<(), RustQueueError> {
        let mut schedule = self
            .storage
            .get_schedule(name)
            .await
            .map_err(RustQueueError::Internal)?
            .ok_or_else(|| RustQueueError::ScheduleNotFound(name.to_string()))?;
        schedule.paused = true;
        schedule.updated_at = Utc::now();
        self.storage
            .upsert_schedule(&schedule)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Resume a paused schedule.
    pub async fn resume_schedule(&self, name: &str) -> Result<(), RustQueueError> {
        let mut schedule = self
            .storage
            .get_schedule(name)
            .await
            .map_err(RustQueueError::Internal)?
            .ok_or_else(|| RustQueueError::ScheduleNotFound(name.to_string()))?;
        schedule.paused = false;
        schedule.updated_at = Utc::now();
        self.storage
            .upsert_schedule(&schedule)
            .await
            .map_err(RustQueueError::Internal)
    }

    // ── Queue pause/resume ────────────────────────────────────────────────

    /// Pause a queue, preventing new jobs from being pushed to it.
    pub fn pause_queue(&self, queue: &str) {
        self.paused_queues.insert(queue.to_string());
        info!(queue, "Queue paused");
    }

    /// Resume a paused queue, allowing new jobs to be pushed.
    pub fn resume_queue(&self, queue: &str) {
        self.paused_queues.remove(queue);
        info!(queue, "Queue resumed");
    }

    /// Check whether a queue is currently paused.
    pub fn is_queue_paused(&self, queue: &str) -> bool {
        self.paused_queues.contains(queue)
    }

    // ── Schedule execution ────────────────────────────────────────────────

    /// Evaluate all active schedules and create jobs for those that are due.
    ///
    /// For each non-paused schedule whose `next_run_at` has passed (or is `None`
    /// for first-run), a new job is pushed to the schedule's target queue.
    /// The schedule's `execution_count`, `last_run_at`, `next_run_at`, and
    /// (optionally) `paused` flag are updated accordingly.
    ///
    /// Returns the number of schedules that fired.
    pub async fn execute_schedules(&self) -> Result<u32, RustQueueError> {
        let schedules = self
            .storage
            .get_active_schedules()
            .await
            .map_err(RustQueueError::Internal)?;
        let now = Utc::now();
        let mut fired = 0u32;

        for mut schedule in schedules {
            let is_due = match schedule.next_run_at {
                Some(next) => next <= now,
                None => true, // First run
            };
            if !is_due {
                continue;
            }

            // Push job
            if let Err(e) = self
                .push(
                    &schedule.queue,
                    &schedule.job_name,
                    schedule.job_data.clone(),
                    schedule.job_options.clone(),
                )
                .await
            {
                tracing::warn!(schedule = %schedule.name, error = %e, "Schedule job push failed");
                continue;
            }

            fired += 1;
            schedule.execution_count += 1;
            schedule.last_run_at = Some(now);
            schedule.updated_at = now;

            // Compute next_run_at
            schedule.next_run_at = compute_next_run(&schedule, now);

            // Check max_executions
            if let Some(max) = schedule.max_executions {
                if schedule.execution_count >= max {
                    schedule.paused = true;
                }
            }

            if let Err(e) = self.storage.upsert_schedule(&schedule).await {
                tracing::warn!(schedule = %schedule.name, error = %e, "Schedule update failed");
            }
        }

        if fired > 0 {
            metrics::counter!(metric_names::SCHEDULES_FIRED_TOTAL).increment(fired as u64);
            tracing::info!(count = fired, "Schedules fired");
        }
        Ok(fired)
    }

    // ── DAG dependency helpers ─────────────────────────────────────────

    /// Check if all dependency parent jobs are in Completed state.
    async fn all_deps_completed(&self, deps: &[JobId]) -> Result<bool, RustQueueError> {
        for &dep_id in deps {
            let dep = self
                .storage
                .get_job(dep_id)
                .await
                .map_err(RustQueueError::Internal)?;
            match dep {
                Some(j) if j.state == JobState::Completed => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// BFS cycle detection + max depth check for a job's dependency chain.
    async fn detect_cycle(&self, job: &Job) -> Result<(), RustQueueError> {
        let mut visited = HashSet::new();
        let mut queue: VecDeque<(JobId, usize)> = VecDeque::new();

        for &parent_id in &job.depends_on {
            queue.push_back((parent_id, 1));
        }

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth > self.max_dag_depth {
                return Err(RustQueueError::ValidationError(format!(
                    "Dependency chain exceeds maximum depth of {}",
                    self.max_dag_depth
                )));
            }
            if current_id == job.id {
                return Err(RustQueueError::ValidationError(
                    "Circular dependency detected".into(),
                ));
            }
            if !visited.insert(current_id) {
                continue; // Already visited
            }

            // Look up this parent's own dependencies
            if let Some(parent) = self
                .storage
                .get_job(current_id)
                .await
                .map_err(RustQueueError::Internal)?
            {
                for &grandparent_id in &parent.depends_on {
                    queue.push_back((grandparent_id, depth + 1));
                }
            }
        }

        Ok(())
    }

    /// After a job completes, check its children in the reverse index.
    /// Promote any Blocked children whose dependencies are all Completed.
    async fn resolve_dependencies(&self, completed_id: JobId) {
        let children = self
            .dependency_index
            .get(&completed_id)
            .map(|r| r.value().clone())
            .unwrap_or_default();

        for child_id in children {
            let child = match self.storage.get_job(child_id).await {
                Ok(Some(j)) => j,
                _ => continue,
            };
            if child.state != JobState::Blocked {
                continue;
            }
            // Check if ALL deps are now Completed
            if let Ok(true) = self.all_deps_completed(&child.depends_on).await {
                let mut child = child;
                child.state = JobState::Waiting;
                child.updated_at = Utc::now();
                if let Err(e) = self.storage.update_job(&child).await {
                    tracing::warn!(
                        job_id = %child_id,
                        error = %e,
                        "Failed to promote blocked job"
                    );
                } else {
                    info!(job_id = %child_id, "Blocked job promoted to Waiting (deps resolved)");
                    self.emit_event("job.pushed", child_id, &child.queue);
                }
            }
        }
    }

    /// Cascade DLQ to all Blocked children of a failed parent, recursively.
    async fn cascade_dependency_failure(&self, failed_id: JobId) {
        let children = self
            .dependency_index
            .get(&failed_id)
            .map(|r| r.value().clone())
            .unwrap_or_default();

        for child_id in children {
            let child = match self.storage.get_job(child_id).await {
                Ok(Some(j)) => j,
                _ => continue,
            };
            if child.state != JobState::Blocked {
                continue;
            }
            let reason = format!("parent job '{}' moved to DLQ", failed_id);
            if let Err(e) = self.storage.move_to_dlq(&child, &reason).await {
                tracing::warn!(
                    job_id = %child_id,
                    error = %e,
                    "Failed to cascade DLQ to blocked child"
                );
            } else {
                self.emit_event("job.failed", child_id, &child.queue);
                info!(job_id = %child_id, parent_id = %failed_id, "Blocked child cascaded to DLQ");
                // Recursively cascade to grandchildren
                Box::pin(self.cascade_dependency_failure(child_id)).await;
            }
        }
    }

    /// Safety net: promote orphaned Blocked jobs whose deps are all Completed.
    ///
    /// Called periodically by the scheduler to catch edge cases where the reverse
    /// index missed a promotion (e.g., after server restart).
    pub async fn promote_orphaned_blocked_jobs(&self) -> Result<u32, RustQueueError> {
        let names = self
            .storage
            .list_queue_names()
            .await
            .map_err(RustQueueError::Internal)?;

        let mut promoted = 0u32;
        for name in names {
            // Get all jobs and filter for Blocked ones
            // This is a scan but only runs every 10 scheduler ticks
            let counts = self
                .storage
                .get_queue_counts(&name)
                .await
                .map_err(RustQueueError::Internal)?;
            if counts.blocked == 0 {
                continue;
            }

            // We need to find blocked jobs. Use get_jobs_by_flow_id if available,
            // or scan via storage. For simplicity, we'll check jobs we know about
            // from the dependency index.
            for entry in self.dependency_index.iter() {
                for &child_id in entry.value() {
                    let child = match self.storage.get_job(child_id).await {
                        Ok(Some(j)) => j,
                        _ => continue,
                    };
                    if child.state != JobState::Blocked || child.queue != name {
                        continue;
                    }
                    if self
                        .all_deps_completed(&child.depends_on)
                        .await
                        .unwrap_or(false)
                    {
                        let mut child = child;
                        child.state = JobState::Waiting;
                        child.updated_at = Utc::now();
                        if self.storage.update_job(&child).await.is_ok() {
                            promoted += 1;
                            self.emit_event("job.pushed", child_id, &child.queue);
                        }
                    }
                }
            }
        }

        if promoted > 0 {
            info!(count = promoted, "Promoted orphaned blocked jobs");
        }
        Ok(promoted)
    }

    /// Get all jobs belonging to a flow.
    pub async fn get_flow_jobs(&self, flow_id: &str) -> Result<Vec<Job>, RustQueueError> {
        self.storage
            .get_jobs_by_flow_id(flow_id)
            .await
            .map_err(RustQueueError::Internal)
    }

    /// Parse follow-up descriptors from `job.metadata.follow_ups`.
    fn extract_follow_up_specs(job: &Job) -> Result<Vec<FollowUpJobSpec>, RustQueueError> {
        let Some(metadata) = job.metadata.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(raw_specs) = metadata.get(FOLLOW_UPS_METADATA_KEY) else {
            return Ok(Vec::new());
        };

        serde_json::from_value::<Vec<FollowUpJobSpec>>(raw_specs.clone()).map_err(|e| {
            RustQueueError::ValidationError(format!(
                "Invalid metadata.{FOLLOW_UPS_METADATA_KEY} on job '{}': {e}",
                job.id
            ))
        })
    }

    /// Prepare metadata to inherit into follow-up jobs, excluding follow-up config itself.
    fn inherited_metadata_without_follow_ups(job: &Job) -> Option<serde_json::Value> {
        let mut inherited = job.metadata.clone()?;
        if let serde_json::Value::Object(ref mut map) = inherited {
            map.remove(FOLLOW_UPS_METADATA_KEY);
            if map.is_empty() {
                return None;
            }
        }
        Some(inherited)
    }

    /// Enqueue follow-up jobs after a parent job completes.
    ///
    /// Follow-ups are declared in `metadata.follow_ups` and are linked to the
    /// parent via `depends_on` and inherited `flow_id`.
    async fn enqueue_follow_ups(&self, parent_job: &Job) {
        let specs = match Self::extract_follow_up_specs(parent_job) {
            Ok(specs) => specs,
            Err(err) => {
                warn!(
                    parent_job_id = %parent_job.id,
                    error = %err,
                    "ignoring invalid follow-up metadata"
                );
                return;
            }
        };

        if specs.is_empty() {
            return;
        }

        let inherited_metadata = Self::inherited_metadata_without_follow_ups(parent_job);
        for spec in specs {
            let mut options = spec.options.unwrap_or_default();

            match options.depends_on.as_mut() {
                Some(deps) => {
                    if !deps.contains(&parent_job.id) {
                        deps.push(parent_job.id);
                    }
                }
                None => options.depends_on = Some(vec![parent_job.id]),
            }

            if options.flow_id.is_none() {
                options.flow_id = spec.flow_id.clone().or_else(|| parent_job.flow_id.clone());
            }

            if options.metadata.is_none() {
                options.metadata = inherited_metadata.clone();
            }

            let data = spec.data.unwrap_or_else(|| {
                parent_job
                    .result
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({}))
            });

            match self
                .push(&spec.queue, &spec.name, data, Some(options))
                .await
            {
                Ok(child_id) => {
                    info!(
                        parent_job_id = %parent_job.id,
                        child_job_id = %child_id,
                        queue = %spec.queue,
                        "Enqueued follow-up job"
                    );
                }
                Err(err) => {
                    warn!(
                        parent_job_id = %parent_job.id,
                        queue = %spec.queue,
                        name = %spec.name,
                        error = %err,
                        "Failed to enqueue follow-up job"
                    );
                }
            }
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Fetch a job by ID, returning `JobNotFound` if it does not exist.
    async fn require_job(&self, id: JobId) -> Result<Job, RustQueueError> {
        self.storage
            .get_job(id)
            .await
            .map_err(RustQueueError::Internal)?
            .ok_or_else(|| RustQueueError::JobNotFound(id.to_string()))
    }

    /// Compute the retry backoff delay in milliseconds.
    fn compute_backoff(strategy: BackoffStrategy, base_delay_ms: u64, attempt: u32) -> u64 {
        match strategy {
            BackoffStrategy::Fixed => base_delay_ms,
            BackoffStrategy::Linear => base_delay_ms * u64::from(attempt),
            BackoffStrategy::Exponential => {
                base_delay_ms * 2u64.saturating_pow(attempt.saturating_sub(1))
            }
        }
    }
}

// ── TTL parsing utility ──────────────────────────────────────────────────────

/// Parse a human-readable TTL string like "7d", "24h", or "30m" into a `chrono::Duration`.
///
/// Returns `None` if the format is invalid.
fn parse_ttl(ttl: &str) -> Option<Duration> {
    let s = ttl.trim();
    if let Some(d) = s.strip_suffix('d') {
        d.parse::<i64>().ok().map(Duration::days)
    } else if let Some(h) = s.strip_suffix('h') {
        h.parse::<i64>().ok().map(Duration::hours)
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<i64>().ok().map(Duration::minutes)
    } else {
        None
    }
}

// ── Schedule next-run computation ─────────────────────────────────────────────

/// Compute the next run time for a schedule based on its cron expression or interval.
///
/// - If the schedule has a `cron_expr`, parses it via `croner` and finds the
///   next occurrence after `after`.
/// - Otherwise, if `every_ms` is set, returns `after + every_ms`.
/// - Returns `None` if neither is configured or the cron expression is invalid.
fn compute_next_run(schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(ref cron_expr) = schedule.cron_expr {
        if let Ok(cron) = croner::Cron::new(cron_expr).parse() {
            return cron.find_next_occurrence(&after, false).ok();
        }
    }
    if let Some(every_ms) = schedule.every_ms {
        return Some(after + Duration::milliseconds(every_ms as i64));
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::RedbStorage;
    use serde_json::json;
    use tempfile::NamedTempFile;

    /// Helper: create a QueueManager backed by a temporary RedbStorage.
    fn temp_manager() -> QueueManager {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        drop(tmp);
        let storage = RedbStorage::new(&path).unwrap();
        QueueManager::new(Arc::new(storage))
    }

    #[tokio::test]
    async fn test_push_job() {
        let mgr = temp_manager();
        let id = mgr
            .push("emails", "send-welcome", json!({"to": "a@b.com"}), None)
            .await
            .unwrap();

        let job = mgr.get_job(id).await.unwrap().expect("job should exist");
        assert_eq!(job.queue, "emails");
        assert_eq!(job.name, "send-welcome");
        assert_eq!(job.state, JobState::Waiting);
        assert_eq!(job.data, json!({"to": "a@b.com"}));
        assert_eq!(job.priority, 0);
        assert_eq!(job.max_attempts, 3);
        assert_eq!(job.attempt, 0);
    }

    #[tokio::test]
    async fn test_push_and_pull() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();

        let pulled = mgr.pull("work", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].id, id);
        assert_eq!(pulled[0].state, JobState::Active);
        assert!(pulled[0].started_at.is_some());
    }

    #[tokio::test]
    async fn test_ack_job() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();

        // Pull to make it Active.
        mgr.pull("work", 1).await.unwrap();

        // Ack with a result.
        mgr.ack(id, Some(json!({"output": "done"}))).await.unwrap();

        let job = mgr.get_job(id).await.unwrap().expect("job should exist");
        assert_eq!(job.state, JobState::Completed);
        assert!(job.completed_at.is_some());
        assert_eq!(job.result, Some(json!({"output": "done"})));
    }

    #[tokio::test]
    async fn test_ack_batch_mixed_results() {
        let mgr = temp_manager();
        let id_ok = mgr
            .push("work", "process-ok", json!({}), None)
            .await
            .unwrap();
        let id_invalid_state = mgr
            .push("work", "process-invalid", json!({}), None)
            .await
            .unwrap();
        let missing_id = uuid::Uuid::now_v7();

        // Activate only one job.
        let pulled = mgr.pull("work", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].id, id_ok);

        let results = mgr
            .ack_batch(vec![
                BatchAckItem {
                    id: id_ok,
                    result: Some(json!({"ok": true})),
                },
                BatchAckItem {
                    id: id_invalid_state,
                    result: None,
                },
                BatchAckItem {
                    id: missing_id,
                    result: None,
                },
            ])
            .await
            .unwrap();

        assert_eq!(results.len(), 3);

        assert!(results[0].ok);
        assert!(!results[1].ok);
        assert_eq!(results[1].error_code.as_deref(), Some("INVALID_STATE"));
        assert!(!results[2].ok);
        assert_eq!(results[2].error_code.as_deref(), Some("JOB_NOT_FOUND"));

        let completed = mgr.get_job(id_ok).await.unwrap().unwrap();
        assert_eq!(completed.state, JobState::Completed);
        assert_eq!(completed.result, Some(json!({"ok": true})));
    }

    #[tokio::test]
    async fn test_fail_job_with_retries() {
        let mgr = temp_manager();
        // Default max_attempts = 3, so after 1 failure it should retry.
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();

        mgr.pull("work", 1).await.unwrap();

        let result = mgr.fail(id, "timeout").await.unwrap();
        assert!(result.will_retry);

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.attempt, 1);
        // With default exponential backoff (base 1000ms), attempt 1 -> delay = 1000 * 2^0 = 1000ms > 0
        // so state should be Delayed.
        assert_eq!(job.state, JobState::Delayed);
        assert!(job.delay_until.is_some());
    }

    #[tokio::test]
    async fn test_fail_job_exhausted_retries_goes_to_dlq() {
        let mgr = temp_manager();
        let opts = JobOptions {
            max_attempts: Some(1),
            ..Default::default()
        };
        let id = mgr
            .push("work", "process", json!({}), Some(opts))
            .await
            .unwrap();

        mgr.pull("work", 1).await.unwrap();

        let result = mgr.fail(id, "fatal error").await.unwrap();
        assert!(!result.will_retry);
        assert!(result.next_attempt_at.is_none());

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Dlq);
    }

    #[tokio::test]
    async fn test_cancel_waiting_job() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();

        mgr.cancel(id).await.unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_active_job_fails() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();

        // Pull to make it Active.
        mgr.pull("work", 1).await.unwrap();

        let err = mgr.cancel(id).await.unwrap_err();
        match err {
            RustQueueError::InvalidState { current, expected } => {
                assert_eq!(current, "Active");
                assert_eq!(expected, "Waiting, Delayed, or Blocked");
            }
            other => panic!("expected InvalidState, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_unique_key_deduplication() {
        let mgr = temp_manager();
        let opts = JobOptions {
            unique_key: Some("user-42-welcome".to_string()),
            ..Default::default()
        };

        mgr.push("emails", "send", json!({}), Some(opts.clone()))
            .await
            .unwrap();

        let err = mgr
            .push("emails", "send", json!({}), Some(opts))
            .await
            .unwrap_err();

        match err {
            RustQueueError::DuplicateKey(key) => assert_eq!(key, "user-42-welcome"),
            other => panic!("expected DuplicateKey, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_delayed_job() {
        let mgr = temp_manager();
        let opts = JobOptions {
            delay_ms: Some(60_000),
            ..Default::default()
        };
        let id = mgr
            .push("work", "process", json!({}), Some(opts))
            .await
            .unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Delayed);
        assert!(job.delay_until.is_some());

        // Delayed job should not be returned by pull (only Waiting jobs are dequeued).
        let pulled = mgr.pull("work", 10).await.unwrap();
        assert!(pulled.is_empty());
    }

    #[tokio::test]
    async fn test_custom_job_id() {
        let mgr = temp_manager();
        let opts = JobOptions {
            custom_id: Some("my-custom-id-123".to_string()),
            ..Default::default()
        };
        let id = mgr
            .push("work", "process", json!({}), Some(opts))
            .await
            .unwrap();
        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.custom_id, Some("my-custom-id-123".to_string()));
    }

    #[tokio::test]
    async fn test_update_progress() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();

        mgr.update_progress(id, 50, Some("halfway there".to_string()))
            .await
            .unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.progress, Some(50));
        assert_eq!(job.logs.len(), 1);
        assert_eq!(job.logs[0].message, "halfway there");
    }

    #[tokio::test]
    async fn test_update_progress_requires_active_state() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        // Job is Waiting, not Active
        let err = mgr.update_progress(id, 50, None).await.unwrap_err();
        match err {
            RustQueueError::InvalidState { current, expected } => {
                assert_eq!(current, "Waiting");
                assert_eq!(expected, "Active");
            }
            other => panic!("expected InvalidState, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_list_queues() {
        let mgr = temp_manager();
        mgr.push("emails", "send", json!({}), None).await.unwrap();
        mgr.push("reports", "generate", json!({}), None)
            .await
            .unwrap();

        let queues = mgr.list_queues().await.unwrap();
        let names: Vec<&str> = queues.iter().map(|q| q.name.as_str()).collect();
        assert!(names.contains(&"emails"));
        assert!(names.contains(&"reports"));
        assert_eq!(queues.len(), 2);

        // Each queue should have 1 waiting job.
        for qi in &queues {
            assert_eq!(qi.counts.waiting, 1);
        }
    }

    #[tokio::test]
    async fn test_check_timeouts() {
        let mgr = temp_manager();
        // Push a job with a very short timeout (1ms)
        let opts = JobOptions {
            timeout_ms: Some(1),
            ..Default::default()
        };
        let id = mgr
            .push("work", "process", json!({}), Some(opts))
            .await
            .unwrap();

        // Pull to make Active
        mgr.pull("work", 1).await.unwrap();

        // Wait a bit for timeout to expire
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Run timeout check
        let count = mgr.check_timeouts().await.unwrap();
        assert_eq!(count, 1);

        // Job should be retried (moved to Delayed or Waiting) since default max_attempts=3
        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert!(
            matches!(job.state, JobState::Delayed | JobState::Waiting),
            "expected Delayed or Waiting, got {:?}",
            job.state
        );
        assert_eq!(job.last_error, Some("job timed out".to_string()));
    }

    #[tokio::test]
    async fn test_check_timeouts_no_timeout_set() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();

        // No timeout set, so check_timeouts should return 0
        let count = mgr.check_timeouts().await.unwrap();
        assert_eq!(count, 0);

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Active);
    }

    #[tokio::test]
    async fn test_heartbeat() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();

        mgr.heartbeat(id).await.unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert!(job.last_heartbeat.is_some());
    }

    #[tokio::test]
    async fn test_heartbeat_requires_active() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        // Job is Waiting, not Active
        let err = mgr.heartbeat(id).await.unwrap_err();
        match err {
            RustQueueError::InvalidState { current, expected } => {
                assert_eq!(current, "Waiting");
                assert_eq!(expected, "Active");
            }
            other => panic!("expected InvalidState, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_detect_stalls() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();

        // Wait a bit, then detect stalls with a very short timeout
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let stalled = mgr.detect_stalls(1).await.unwrap(); // 1ms timeout
        assert_eq!(stalled, 1);

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert!(
            matches!(job.state, JobState::Delayed | JobState::Waiting),
            "expected Delayed or Waiting, got {:?}",
            job.state
        );
        assert_eq!(
            job.last_error,
            Some("job stalled (no heartbeat)".to_string())
        );
    }

    #[tokio::test]
    async fn test_promote_delayed_jobs() {
        let mgr = temp_manager();

        // Push a delayed job with a very short delay (1ms)
        let opts = JobOptions {
            delay_ms: Some(1),
            ..Default::default()
        };
        let id = mgr
            .push("work", "process", json!({}), Some(opts))
            .await
            .unwrap();

        // Verify it's in Delayed state
        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Delayed);

        // Wait a tiny bit to ensure the delay has expired
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // Promote
        let count = mgr.promote_delayed_jobs().await.unwrap();
        assert_eq!(count, 1);

        // Now it should be Waiting
        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Waiting);
        assert!(job.delay_until.is_none());

        // And pullable
        let pulled = mgr.pull("work", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].id, id);
    }

    // ── Retention / TTL tests ────────────────────────────────────────────

    #[test]
    fn test_parse_ttl_formats() {
        use super::parse_ttl;

        // Valid formats
        assert_eq!(parse_ttl("7d"), Some(Duration::days(7)));
        assert_eq!(parse_ttl("24h"), Some(Duration::hours(24)));
        assert_eq!(parse_ttl("30m"), Some(Duration::minutes(30)));
        assert_eq!(parse_ttl("1d"), Some(Duration::days(1)));
        assert_eq!(parse_ttl("90d"), Some(Duration::days(90)));

        // Whitespace trimming
        assert_eq!(parse_ttl("  7d  "), Some(Duration::days(7)));

        // Invalid formats
        assert_eq!(parse_ttl("invalid"), None);
        assert_eq!(parse_ttl("7"), None);
        assert_eq!(parse_ttl(""), None);
        assert_eq!(parse_ttl("7s"), None); // seconds not supported
        assert_eq!(parse_ttl("abc_d"), None);
    }

    #[tokio::test]
    async fn test_cleanup_removes_old_completed() {
        let mgr = temp_manager();

        // Push a job, pull it, ack it so it becomes Completed.
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();
        mgr.ack(id, None).await.unwrap();

        // Manually backdate the completed_at to 30 days ago to simulate an old job.
        let mut job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Completed);
        job.completed_at = Some(Utc::now() - Duration::days(30));
        job.updated_at = Utc::now() - Duration::days(30);
        mgr.storage.update_job(&job).await.unwrap();

        // Run cleanup with a 7-day TTL — the 30-day-old job should be removed.
        let (completed, failed, dlq) = mgr.cleanup_expired_jobs("7d", "30d", "90d").await.unwrap();
        assert_eq!(completed, 1);
        assert_eq!(failed, 0);
        assert_eq!(dlq, 0);

        // Verify the job is gone.
        let job = mgr.get_job(id).await.unwrap();
        assert!(job.is_none(), "old completed job should have been removed");
    }

    // ── Schedule tests ────────────────────────────────────────────────

    /// Helper: create a QueueManager backed by MemoryStorage.
    fn memory_manager() -> QueueManager {
        use crate::storage::MemoryStorage;
        let storage = Arc::new(MemoryStorage::new());
        QueueManager::new(storage)
    }

    /// Helper: create a test Schedule with the given cron_expr and every_ms.
    fn test_schedule(
        cron_expr: Option<&str>,
        every_ms: Option<u64>,
    ) -> crate::engine::models::Schedule {
        crate::engine::models::Schedule {
            name: "test-schedule".to_string(),
            queue: "emails".to_string(),
            job_name: "send-welcome".to_string(),
            job_data: json!({}),
            job_options: None,
            cron_expr: cron_expr.map(|s| s.to_string()),
            every_ms,
            timezone: None,
            max_executions: None,
            execution_count: 0,
            paused: false,
            last_run_at: None,
            next_run_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_create_schedule_validates_cron() {
        let mgr = memory_manager();
        let schedule = test_schedule(Some("not-a-cron"), None);

        let err = mgr.create_schedule(&schedule).await.unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(
                    msg.contains("Invalid cron expression"),
                    "expected 'Invalid cron expression' in: {msg}"
                );
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_create_schedule_requires_timing() {
        let mgr = memory_manager();
        let schedule = test_schedule(None, None);

        let err = mgr.create_schedule(&schedule).await.unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(
                    msg.contains("must have cron_expr or every_ms"),
                    "expected timing requirement message in: {msg}"
                );
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pause_resume_schedule() {
        let mgr = memory_manager();
        let schedule = test_schedule(Some("*/5 * * * *"), None);

        // Create the schedule.
        mgr.create_schedule(&schedule).await.unwrap();

        // Pause it.
        mgr.pause_schedule("test-schedule").await.unwrap();
        let paused = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert!(paused.paused, "schedule should be paused");

        // Resume it.
        mgr.resume_schedule("test-schedule").await.unwrap();
        let resumed = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert!(!resumed.paused, "schedule should not be paused");
    }

    #[tokio::test]
    async fn test_cleanup_preserves_recent() {
        let mgr = temp_manager();

        // Push, pull, and ack a job — it becomes Completed just now.
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();
        mgr.ack(id, None).await.unwrap();

        // Run cleanup with 7d TTL — recent job should survive.
        let (completed, failed, dlq) = mgr.cleanup_expired_jobs("7d", "30d", "90d").await.unwrap();
        assert_eq!(completed, 0);
        assert_eq!(failed, 0);
        assert_eq!(dlq, 0);

        // The job should still exist.
        let job = mgr.get_job(id).await.unwrap();
        assert!(job.is_some(), "recent completed job should be preserved");
    }

    // ── Schedule execution tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_interval_schedule() {
        let mgr = memory_manager();

        // Create a schedule with a 100ms interval.
        let schedule = test_schedule(None, Some(100));
        mgr.create_schedule(&schedule).await.unwrap();

        // First execution — next_run_at is None so it fires immediately.
        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 1, "schedule should fire on first call");

        // Verify a job was created.
        let counts = mgr.get_queue_stats("emails").await.unwrap();
        assert_eq!(counts.waiting, 1, "one job should be waiting");

        // Verify schedule was updated.
        let s = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert_eq!(s.execution_count, 1);
        assert!(s.last_run_at.is_some());
        assert!(s.next_run_at.is_some());

        // Wait for the interval to elapse, then fire again.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 1, "schedule should fire again after interval");

        let counts = mgr.get_queue_stats("emails").await.unwrap();
        assert_eq!(counts.waiting, 2, "two jobs should be waiting");

        let s = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert_eq!(s.execution_count, 2);
    }

    #[tokio::test]
    async fn test_max_executions_pauses_schedule() {
        let mgr = memory_manager();

        // Create a schedule with max_executions = 2 and a 100ms interval.
        let mut schedule = test_schedule(None, Some(100));
        schedule.max_executions = Some(2);
        mgr.create_schedule(&schedule).await.unwrap();

        // First call fires (execution_count becomes 1).
        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 1);

        // Wait for interval then fire again (execution_count becomes 2, hits max).
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 1);

        // Schedule should now be paused.
        let s = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert_eq!(s.execution_count, 2);
        assert!(s.paused, "schedule should be paused after max_executions");

        // Third call should not fire (schedule is paused, get_active_schedules skips it).
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 0, "paused schedule should not fire");

        // Verify only 2 jobs were created total.
        let counts = mgr.get_queue_stats("emails").await.unwrap();
        assert_eq!(counts.waiting, 2, "only 2 jobs should have been created");
    }

    #[tokio::test]
    async fn test_execute_cron_schedule() {
        let mgr = memory_manager();

        // Create a schedule with a cron expression that runs every minute.
        let schedule = test_schedule(Some("* * * * *"), None);
        mgr.create_schedule(&schedule).await.unwrap();

        // First execution — next_run_at is None so it fires immediately.
        let fired = mgr.execute_schedules().await.unwrap();
        assert_eq!(fired, 1);

        // Verify schedule state after firing.
        let s = mgr
            .get_schedule("test-schedule")
            .await
            .unwrap()
            .expect("schedule should exist");
        assert_eq!(s.execution_count, 1);
        assert!(s.last_run_at.is_some());
        assert!(s.next_run_at.is_some(), "next_run_at should be computed");

        // The next_run_at should be in the future (at least ~1 minute from now).
        let next = s.next_run_at.unwrap();
        assert!(
            next > Utc::now(),
            "next_run_at should be in the future, got: {next}"
        );
    }

    #[tokio::test]
    async fn test_compute_next_run_interval() {
        let schedule = test_schedule(None, Some(5000));
        let now = Utc::now();
        let next = super::compute_next_run(&schedule, now);
        assert!(next.is_some());
        let expected = now + Duration::milliseconds(5000);
        assert_eq!(next.unwrap(), expected);
    }

    #[tokio::test]
    async fn test_compute_next_run_cron() {
        let schedule = test_schedule(Some("0 12 * * *"), None);
        let now = Utc::now();
        let next = super::compute_next_run(&schedule, now);
        assert!(next.is_some());
        // The next occurrence of "0 12 * * *" should be in the future.
        assert!(next.unwrap() > now);
    }

    #[tokio::test]
    async fn test_compute_next_run_neither() {
        // A schedule with neither cron nor interval — should return None.
        let mut schedule = test_schedule(None, Some(100));
        schedule.every_ms = None; // Clear it to test the None path
        let now = Utc::now();
        let next = super::compute_next_run(&schedule, now);
        assert!(next.is_none());
    }

    // ── Input validation tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_push_validates_queue_name_chars() {
        let mgr = temp_manager();
        let err = mgr
            .push("my queue!", "job", json!({}), None)
            .await
            .unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("alphanumeric"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_push_validates_queue_name_empty() {
        let mgr = temp_manager();
        let err = mgr.push("", "job", json!({}), None).await.unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("non-empty"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_push_validates_job_name_length() {
        let mgr = temp_manager();
        let long_name = "a".repeat(super::MAX_JOB_NAME_LEN + 1);
        let err = mgr
            .push("queue", &long_name, json!({}), None)
            .await
            .unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("exceeds maximum"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_push_validates_data_size() {
        let mgr = temp_manager();
        // Create a JSON value larger than 1MB
        let big_str = "x".repeat(super::MAX_JOB_DATA_SIZE + 1);
        let data = json!({"huge": big_str});
        let err = mgr.push("queue", "job", data, None).await.unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("payload"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_push_validates_unique_key_length() {
        let mgr = temp_manager();
        let long_key = "k".repeat(super::MAX_UNIQUE_KEY_LEN + 1);
        let opts = JobOptions {
            unique_key: Some(long_key),
            ..Default::default()
        };
        let err = mgr
            .push("queue", "job", json!({}), Some(opts))
            .await
            .unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("Unique key"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_fail_validates_error_message_length() {
        let mgr = temp_manager();
        let id = mgr.push("work", "process", json!({}), None).await.unwrap();
        mgr.pull("work", 1).await.unwrap();

        let long_error = "e".repeat(super::MAX_ERROR_MESSAGE_LEN + 1);
        let err = mgr.fail(id, &long_error).await.unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("Error message"), "got: {msg}");
            }
            other => panic!("expected ValidationError, got: {:?}", other),
        }
    }

    // ── Queue pause/resume tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_pause_queue_rejects_push() {
        let mgr = memory_manager();
        mgr.pause_queue("emails");

        let err = mgr
            .push("emails", "send", json!({}), None)
            .await
            .unwrap_err();
        match err {
            RustQueueError::QueuePaused(q) => assert_eq!(q, "emails"),
            other => panic!("expected QueuePaused, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_resume_queue_allows_push() {
        let mgr = memory_manager();
        mgr.pause_queue("emails");
        mgr.resume_queue("emails");

        let id = mgr.push("emails", "send", json!({}), None).await.unwrap();
        let job = mgr.get_job(id).await.unwrap().expect("job should exist");
        assert_eq!(job.queue, "emails");
    }

    #[tokio::test]
    async fn test_pause_queue_rejects_batch() {
        let mgr = memory_manager();
        mgr.pause_queue("work");

        let items = vec![BatchPushItem {
            name: "job".to_string(),
            data: json!({}),
            options: None,
        }];
        let err = mgr.push_batch("work", items).await.unwrap_err();
        match err {
            RustQueueError::QueuePaused(q) => assert_eq!(q, "work"),
            other => panic!("expected QueuePaused, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_is_queue_paused() {
        let mgr = memory_manager();
        assert!(!mgr.is_queue_paused("emails"));
        mgr.pause_queue("emails");
        assert!(mgr.is_queue_paused("emails"));
        mgr.resume_queue("emails");
        assert!(!mgr.is_queue_paused("emails"));
    }

    #[tokio::test]
    async fn test_valid_names_accepted() {
        let mgr = temp_manager();
        // These should all pass validation
        mgr.push("emails", "send-welcome", json!({}), None)
            .await
            .unwrap();
        mgr.push("my_queue", "job.v2", json!({}), None)
            .await
            .unwrap();
        mgr.push("queue123", "my_job_name", json!({}), None)
            .await
            .unwrap();
    }
}
