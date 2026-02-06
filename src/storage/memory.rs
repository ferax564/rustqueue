//! In-memory storage backend for tests and ephemeral queues.
//!
//! Uses `std::sync::RwLock` (not tokio) since all operations are CPU-bound
//! in-memory HashMap lookups/inserts. Data is cloned in and out.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::StorageBackend;

/// In-memory storage backend — useful for tests and short-lived processes.
///
/// All data lives in `HashMap`s behind `std::sync::RwLock`. Data is lost when
/// the `MemoryStorage` is dropped.
pub struct MemoryStorage {
    jobs: RwLock<HashMap<JobId, Job>>,
    schedules: RwLock<HashMap<String, Schedule>>,
}

impl MemoryStorage {
    /// Create a new, empty in-memory storage.
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
            schedules: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StorageBackend for MemoryStorage {
    // ── Job CRUD ────────────────────────────────────────────────────────

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let id = job.id;
        let mut jobs = self.jobs.write().unwrap();
        jobs.insert(id, job.clone());
        Ok(id)
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        let jobs = self.jobs.read().unwrap();
        Ok(jobs.get(&id).cloned())
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        let mut jobs = self.jobs.write().unwrap();
        jobs.insert(job.id, job.clone());
        Ok(())
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        let mut jobs = self.jobs.write().unwrap();
        jobs.remove(&id);
        Ok(())
    }

    // ── Queue operations ────────────────────────────────────────────────

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        let mut jobs = self.jobs.write().unwrap();

        // Collect candidates: matching queue + Waiting state.
        let mut candidates: Vec<JobId> = jobs
            .values()
            .filter(|j| j.queue == queue && j.state == JobState::Waiting)
            .map(|j| j.id)
            .collect();

        // Sort: priority DESC, then created_at ASC (FIFO tiebreaker).
        candidates.sort_by(|a_id, b_id| {
            let a = &jobs[a_id];
            let b = &jobs[b_id];
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });

        // Take up to `count` and transition to Active.
        let now = Utc::now();
        let selected: Vec<Job> = candidates
            .into_iter()
            .take(count as usize)
            .map(|id| {
                let job = jobs.get_mut(&id).unwrap();
                job.state = JobState::Active;
                job.started_at = Some(now);
                job.updated_at = now;
                job.clone()
            })
            .collect();

        Ok(selected)
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        let jobs = self.jobs.read().unwrap();

        let mut counts = QueueCounts::default();
        for job in jobs.values() {
            if job.queue != queue {
                continue;
            }
            match job.state {
                JobState::Waiting | JobState::Created => counts.waiting += 1,
                JobState::Active => counts.active += 1,
                JobState::Delayed => counts.delayed += 1,
                JobState::Completed => counts.completed += 1,
                JobState::Failed => counts.failed += 1,
                JobState::Dlq => counts.dlq += 1,
                // Cancelled, Blocked are not tracked in QueueCounts for v0.1.
                _ => {}
            }
        }
        Ok(counts)
    }

    // ── Scheduled jobs ──────────────────────────────────────────────────

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        let jobs = self.jobs.read().unwrap();

        let ready = jobs
            .values()
            .filter(|j| {
                j.state == JobState::Delayed
                    && j.delay_until
                        .map(|delay_until| delay_until <= now)
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        Ok(ready)
    }

    // ── DLQ ─────────────────────────────────────────────────────────────

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        let mut jobs = self.jobs.write().unwrap();

        let mut updated = job.clone();
        updated.state = JobState::Dlq;
        updated.last_error = Some(reason.to_string());
        updated.updated_at = Utc::now();

        jobs.insert(updated.id, updated);
        Ok(())
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        let jobs = self.jobs.read().unwrap();

        let dlq_jobs: Vec<Job> = jobs
            .values()
            .filter(|j| j.queue == queue && j.state == JobState::Dlq)
            .take(limit as usize)
            .cloned()
            .collect();

        Ok(dlq_jobs)
    }

    // ── Cleanup ─────────────────────────────────────────────────────────

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let mut jobs = self.jobs.write().unwrap();

        let to_remove: Vec<JobId> = jobs
            .values()
            .filter(|j| {
                j.state == JobState::Completed
                    && j.completed_at
                        .map(|completed_at| completed_at < before)
                        .unwrap_or(false)
            })
            .map(|j| j.id)
            .collect();

        let count = to_remove.len() as u64;
        for id in to_remove {
            jobs.remove(&id);
        }

        Ok(count)
    }

    // ── Cron schedules ──────────────────────────────────────────────────

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        let mut schedules = self.schedules.write().unwrap();
        schedules.insert(schedule.name.clone(), schedule.clone());
        Ok(())
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        let schedules = self.schedules.read().unwrap();

        let active = schedules.values().filter(|s| !s.paused).cloned().collect();

        Ok(active)
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        let mut schedules = self.schedules.write().unwrap();
        schedules.remove(name);
        Ok(())
    }

    // ── Discovery ────────────────────────────────────────────────────────

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        let jobs = self.jobs.read().unwrap();

        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for job in jobs.values() {
            names.insert(job.queue.clone());
        }

        Ok(names.into_iter().collect())
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        let jobs = self.jobs.read().unwrap();

        for job in jobs.values() {
            if job.queue == queue
                && job.unique_key.as_deref() == Some(key)
                && !matches!(
                    job.state,
                    JobState::Completed | JobState::Dlq | JobState::Cancelled
                )
            {
                return Ok(Some(job.clone()));
            }
        }

        Ok(None)
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        let jobs = self.jobs.read().unwrap();
        Ok(jobs
            .values()
            .filter(|j| j.state == JobState::Active)
            .cloned()
            .collect())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    /// Helper: create a test job in the given queue.
    fn test_job(queue: &str) -> Job {
        Job::new(queue, "test-job", json!({"key": "value"}))
    }

    #[tokio::test]
    async fn test_insert_and_get_job() {
        let storage = MemoryStorage::new();
        let job = test_job("emails");

        let id = storage.insert_job(&job).await.unwrap();
        assert_eq!(id, job.id);

        let retrieved = storage
            .get_job(id)
            .await
            .unwrap()
            .expect("job should exist");
        assert_eq!(retrieved.id, job.id);
        assert_eq!(retrieved.queue, "emails");
        assert_eq!(retrieved.name, "test-job");
        assert_eq!(retrieved.state, JobState::Waiting);
        assert_eq!(retrieved.data, json!({"key": "value"}));
        assert_eq!(retrieved.priority, 0);
        assert_eq!(retrieved.max_attempts, 3);
    }

    #[tokio::test]
    async fn test_get_nonexistent_returns_none() {
        let storage = MemoryStorage::new();
        let fake_id = uuid::Uuid::now_v7();
        let result = storage.get_job(fake_id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_dequeue_fifo_and_priority() {
        let storage = MemoryStorage::new();

        // Insert three jobs: two with same priority, one with higher priority.
        let mut low1 = test_job("work");
        low1.priority = 1;

        let mut low2 = test_job("work");
        low2.priority = 1;
        // Ensure low2 has a later created_at for deterministic FIFO ordering.
        low2.created_at = low1.created_at + Duration::seconds(1);

        let mut high = test_job("work");
        high.priority = 10;
        // Give high an even later created_at to prove priority wins over FIFO.
        high.created_at = low1.created_at + Duration::seconds(5);

        storage.insert_job(&low1).await.unwrap();
        storage.insert_job(&low2).await.unwrap();
        storage.insert_job(&high).await.unwrap();

        // Dequeue all 3 — should be ordered: high (pri 10), low1 (pri 1, earlier), low2 (pri 1, later).
        let dequeued = storage.dequeue("work", 3).await.unwrap();
        assert_eq!(dequeued.len(), 3);

        assert_eq!(
            dequeued[0].id, high.id,
            "highest priority job should come first"
        );
        assert_eq!(
            dequeued[1].id, low1.id,
            "among same priority, earlier job should come first (FIFO)"
        );
        assert_eq!(
            dequeued[2].id, low2.id,
            "among same priority, later job should come last"
        );

        // All dequeued jobs should be Active with started_at set.
        for job in &dequeued {
            assert_eq!(job.state, JobState::Active);
            assert!(job.started_at.is_some());
        }

        // Verify the state is persisted in storage.
        let stored = storage.get_job(high.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Active);
    }
}
