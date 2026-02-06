//! Core queue manager — high-level business logic for job lifecycle operations.
//!
//! [`QueueManager`] wraps an `Arc<dyn StorageBackend>` and exposes push, pull,
//! ack, fail, cancel, and query operations with proper state-machine validation.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use tracing::info;

use crate::api::websocket::JobEvent;
use crate::engine::error::RustQueueError;
use crate::engine::metrics as metric_names;
use crate::engine::models::{BackoffStrategy, Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::StorageBackend;

// ── Public types ─────────────────────────────────────────────────────────────

/// Options that can be supplied when pushing a new job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

/// Result returned by [`QueueManager::fail`] to indicate retry disposition.
#[derive(Debug)]
pub struct FailResult {
    pub will_retry: bool,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// High-level information about a single queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueInfo {
    pub name: String,
    pub counts: QueueCounts,
}

// ── QueueManager ─────────────────────────────────────────────────────────────

/// Core engine that mediates between callers and the storage backend.
pub struct QueueManager {
    storage: Arc<dyn StorageBackend>,
    event_tx: Option<tokio::sync::broadcast::Sender<JobEvent>>,
}

impl QueueManager {
    /// Create a new `QueueManager` backed by the given storage implementation.
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self {
        Self {
            storage,
            event_tx: None,
        }
    }

    /// Attach a broadcast sender for emitting real-time job events.
    ///
    /// Must be called **before** wrapping the manager in `Arc`.
    pub fn with_event_sender(mut self, tx: tokio::sync::broadcast::Sender<JobEvent>) -> Self {
        self.event_tx = Some(tx);
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
        let mut job = Job::new(queue, name, data);

        // Apply optional overrides.
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

            // Delay handling — must come after unique_key is applied.
            if let Some(delay_ms) = opts.delay_ms {
                job.delay_until = Some(Utc::now() + Duration::milliseconds(delay_ms as i64));
                job.state = JobState::Delayed;
            }
        }

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

        let id = self
            .storage
            .insert_job(&job)
            .await
            .map_err(RustQueueError::Internal)?;

        metrics::counter!(metric_names::JOBS_PUSHED_TOTAL).increment(1);

        self.emit_event("job.pushed", id, queue);

        info!(job_id = %id, "Job pushed");

        Ok(id)
    }

    // ── Pull ─────────────────────────────────────────────────────────────

    /// Dequeue up to `count` jobs from the given queue.
    ///
    /// Returned jobs are transitioned to [`JobState::Active`] atomically.
    #[tracing::instrument(skip(self), fields(queue, count))]
    pub async fn pull(&self, queue: &str, count: u32) -> Result<Vec<Job>, RustQueueError> {
        let jobs = self
            .storage
            .dequeue(queue, count)
            .await
            .map_err(RustQueueError::Internal)?;

        if !jobs.is_empty() {
            metrics::counter!(metric_names::JOBS_PULLED_TOTAL).increment(jobs.len() as u64);
        }

        Ok(jobs)
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
        let mut job = self.require_job(id).await?;

        if job.state != JobState::Active {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Active".to_string(),
            });
        }

        let now = Utc::now();
        job.state = JobState::Completed;
        job.completed_at = Some(now);
        job.updated_at = now;
        job.result = result;

        if job.remove_on_complete {
            self.storage
                .delete_job(id)
                .await
                .map_err(RustQueueError::Internal)?;
        } else {
            self.storage
                .update_job(&job)
                .await
                .map_err(RustQueueError::Internal)?;
        }

        metrics::counter!(metric_names::JOBS_COMPLETED_TOTAL).increment(1);

        self.emit_event("job.completed", id, &job.queue);

        info!(job_id = %id, "Job acknowledged");

        Ok(())
    }

    // ── Fail ─────────────────────────────────────────────────────────────

    /// Report a job failure. The engine decides whether to retry or move to DLQ.
    #[tracing::instrument(skip(self), fields(id = %id, error))]
    pub async fn fail(
        &self,
        id: JobId,
        error: &str,
    ) -> Result<FailResult, RustQueueError> {
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

        if !matches!(job.state, JobState::Waiting | JobState::Delayed) {
            return Err(RustQueueError::InvalidState {
                current: format!("{:?}", job.state),
                expected: "Waiting or Delayed".to_string(),
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
            tracing::info!(count = fired, "Schedules fired");
        }
        Ok(fired)
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();

        let pulled = mgr.pull("work", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].id, id);
        assert_eq!(pulled[0].state, JobState::Active);
        assert!(pulled[0].started_at.is_some());
    }

    #[tokio::test]
    async fn test_ack_job() {
        let mgr = temp_manager();
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();

        // Pull to make it Active.
        mgr.pull("work", 1).await.unwrap();

        // Ack with a result.
        mgr.ack(id, Some(json!({"output": "done"})))
            .await
            .unwrap();

        let job = mgr.get_job(id).await.unwrap().expect("job should exist");
        assert_eq!(job.state, JobState::Completed);
        assert!(job.completed_at.is_some());
        assert_eq!(job.result, Some(json!({"output": "done"})));
    }

    #[tokio::test]
    async fn test_fail_job_with_retries() {
        let mgr = temp_manager();
        // Default max_attempts = 3, so after 1 failure it should retry.
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();

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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();

        mgr.cancel(id).await.unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_active_job_fails() {
        let mgr = temp_manager();
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();

        // Pull to make it Active.
        mgr.pull("work", 1).await.unwrap();

        let err = mgr.cancel(id).await.unwrap_err();
        match err {
            RustQueueError::InvalidState { current, expected } => {
                assert_eq!(current, "Active");
                assert_eq!(expected, "Waiting or Delayed");
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
        mgr.pull("work", 1).await.unwrap();

        mgr.heartbeat(id).await.unwrap();

        let job = mgr.get_job(id).await.unwrap().unwrap();
        assert!(job.last_heartbeat.is_some());
    }

    #[tokio::test]
    async fn test_heartbeat_requires_active() {
        let mgr = temp_manager();
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
        mgr.pull("work", 1).await.unwrap();
        mgr.ack(id, None).await.unwrap();

        // Manually backdate the completed_at to 30 days ago to simulate an old job.
        let mut job = mgr.get_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Completed);
        job.completed_at = Some(Utc::now() - Duration::days(30));
        job.updated_at = Utc::now() - Duration::days(30);
        mgr.storage
            .update_job(&job)
            .await
            .unwrap();

        // Run cleanup with a 7-day TTL — the 30-day-old job should be removed.
        let (completed, failed, dlq) = mgr
            .cleanup_expired_jobs("7d", "30d", "90d")
            .await
            .unwrap();
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
        let id = mgr
            .push("work", "process", json!({}), None)
            .await
            .unwrap();
        mgr.pull("work", 1).await.unwrap();
        mgr.ack(id, None).await.unwrap();

        // Run cleanup with 7d TTL — recent job should survive.
        let (completed, failed, dlq) = mgr
            .cleanup_expired_jobs("7d", "30d", "90d")
            .await
            .unwrap();
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
}
