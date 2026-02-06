//! Core queue manager — high-level business logic for job lifecycle operations.
//!
//! [`QueueManager`] wraps an `Arc<dyn StorageBackend>` and exposes push, pull,
//! ack, fail, cancel, and query operations with proper state-machine validation.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use tracing::info;

use crate::engine::error::RustQueueError;
use crate::engine::metrics as metric_names;
use crate::engine::models::{BackoffStrategy, Job, JobId, JobState, QueueCounts};
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
}

impl QueueManager {
    /// Create a new `QueueManager` backed by the given storage implementation.
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self {
        Self { storage }
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
}
