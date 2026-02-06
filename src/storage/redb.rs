//! Embedded storage backend powered by [redb](https://docs.rs/redb).
//!
//! Uses two tables with byte key/value pairs:
//! - `JOBS_TABLE` — job ID (16 bytes UUID) -> JSON-serialized `Job`
//! - `SCHEDULES_TABLE` — schedule name (UTF-8 bytes) -> JSON-serialized `Schedule`

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable, TableDefinition};

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::StorageBackend;

/// Main job storage: key = UUID bytes (16), value = JSON bytes.
const JOBS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("jobs");

/// Schedule storage: key = schedule name bytes, value = JSON bytes.
const SCHEDULES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("schedules");

/// Embedded storage backend using redb — a pure-Rust, ACID, embedded key-value store.
///
/// All operations are synchronous under the hood; the async trait methods call
/// redb directly without `spawn_blocking` since redb operations are fast for v0.1.
pub struct RedbStorage {
    db: Database,
}

impl RedbStorage {
    /// Create or open a redb database at the given path.
    ///
    /// On first open the required tables are created automatically.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path.as_ref())
            .with_context(|| format!("failed to open redb at {:?}", path.as_ref()))?;

        // Ensure tables exist by opening a write transaction.
        let write_txn = db.begin_write()?;
        {
            let _jobs = write_txn.open_table(JOBS_TABLE)?;
            let _schedules = write_txn.open_table(SCHEDULES_TABLE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }
}

#[async_trait]
impl StorageBackend for RedbStorage {
    // ── Job CRUD ────────────────────────────────────────────────────────

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let id = job.id;
        let key = id.as_bytes().as_slice();
        let value = serde_json::to_vec(job).context("failed to serialize job")?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(id)
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        let key = id.as_bytes().as_slice();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        match table.get(key)? {
            Some(value) => {
                let job: Job =
                    serde_json::from_slice(value.value()).context("failed to deserialize job")?;
                Ok(Some(job))
            }
            None => Ok(None),
        }
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        let key = job.id.as_bytes().as_slice();
        let value = serde_json::to_vec(job).context("failed to serialize job")?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        let key = id.as_bytes().as_slice();
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.remove(key)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // ── Queue operations ────────────────────────────────────────────────

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        let write_txn = self.db.begin_write()?;
        let result = {
            let mut table = write_txn.open_table(JOBS_TABLE)?;

            // Scan all jobs, filter by queue + Waiting state.
            let mut candidates: Vec<Job> = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())?;
                if job.queue == queue && job.state == JobState::Waiting {
                    candidates.push(job);
                }
            }

            // Sort: priority DESC, then created_at ASC (FIFO tiebreaker).
            candidates.sort_by(|a, b| {
                b.priority
                    .cmp(&a.priority)
                    .then_with(|| a.created_at.cmp(&b.created_at))
            });

            // Take up to `count` and transition to Active.
            let now = Utc::now();
            let selected: Vec<Job> = candidates
                .into_iter()
                .take(count as usize)
                .map(|mut job| {
                    job.state = JobState::Active;
                    job.started_at = Some(now);
                    job.updated_at = now;
                    job
                })
                .collect();

            // Write updated jobs back.
            for job in &selected {
                let key = job.id.as_bytes().as_slice();
                let value = serde_json::to_vec(job)?;
                table.insert(key, value.as_slice())?;
            }

            selected
        };
        write_txn.commit()?;
        Ok(result)
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        let mut counts = QueueCounts::default();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
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
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        let mut ready = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
            if job.state == JobState::Delayed {
                if let Some(delay_until) = job.delay_until {
                    if delay_until <= now {
                        ready.push(job);
                    }
                }
            }
        }
        Ok(ready)
    }

    // ── DLQ ─────────────────────────────────────────────────────────────

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        let mut updated = job.clone();
        updated.state = JobState::Dlq;
        updated.last_error = Some(reason.to_string());
        updated.updated_at = Utc::now();

        let key = updated.id.as_bytes().as_slice();
        let value = serde_json::to_vec(&updated)?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        let mut dlq_jobs = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
            if job.queue == queue && job.state == JobState::Dlq {
                dlq_jobs.push(job);
                if dlq_jobs.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(dlq_jobs)
    }

    // ── Cleanup ─────────────────────────────────────────────────────────

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(JOBS_TABLE)?;

            // Collect IDs to remove first (cannot mutate while iterating).
            let mut to_remove: Vec<[u8; 16]> = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())?;
                if job.state == JobState::Completed {
                    if let Some(completed_at) = job.completed_at {
                        if completed_at < before {
                            let mut id_bytes = [0u8; 16];
                            id_bytes.copy_from_slice(key.value());
                            to_remove.push(id_bytes);
                        }
                    }
                }
            }

            let count = to_remove.len() as u64;
            for id_bytes in &to_remove {
                table.remove(id_bytes.as_slice())?;
            }
            count
        };
        write_txn.commit()?;
        Ok(removed)
    }

    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(JOBS_TABLE)?;

            let mut to_remove: Vec<[u8; 16]> = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())?;
                if job.state == JobState::Failed && job.updated_at < before {
                    let mut id_bytes = [0u8; 16];
                    id_bytes.copy_from_slice(key.value());
                    to_remove.push(id_bytes);
                }
            }

            let count = to_remove.len() as u64;
            for id_bytes in &to_remove {
                table.remove(id_bytes.as_slice())?;
            }
            count
        };
        write_txn.commit()?;
        Ok(removed)
    }

    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(JOBS_TABLE)?;

            let mut to_remove: Vec<[u8; 16]> = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())?;
                if job.state == JobState::Dlq && job.updated_at < before {
                    let mut id_bytes = [0u8; 16];
                    id_bytes.copy_from_slice(key.value());
                    to_remove.push(id_bytes);
                }
            }

            let count = to_remove.len() as u64;
            for id_bytes in &to_remove {
                table.remove(id_bytes.as_slice())?;
            }
            count
        };
        write_txn.commit()?;
        Ok(removed)
    }

    // ── Cron schedules ──────────────────────────────────────────────────

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        let key = schedule.name.as_bytes();
        let value = serde_json::to_vec(schedule).context("failed to serialize schedule")?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCHEDULES_TABLE)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SCHEDULES_TABLE)?;

        let mut schedules = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let schedule: Schedule = serde_json::from_slice(value.value())?;
            if !schedule.paused {
                schedules.push(schedule);
            }
        }
        Ok(schedules)
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        let key = name.as_bytes();
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCHEDULES_TABLE)?;
            table.remove(key)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>> {
        let key = name.as_bytes();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SCHEDULES_TABLE)?;

        match table.get(key)? {
            Some(value) => {
                let schedule: Schedule = serde_json::from_slice(value.value())
                    .context("failed to deserialize schedule")?;
                Ok(Some(schedule))
            }
            None => Ok(None),
        }
    }

    async fn list_all_schedules(&self) -> Result<Vec<Schedule>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SCHEDULES_TABLE)?;

        let mut schedules = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let schedule: Schedule = serde_json::from_slice(value.value())?;
            schedules.push(schedule);
        }
        Ok(schedules)
    }

    // ── Discovery ────────────────────────────────────────────────────────

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        let mut names = std::collections::BTreeSet::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
            names.insert(job.queue);
        }
        Ok(names.into_iter().collect())
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;

        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
            if job.queue == queue
                && job.unique_key.as_deref() == Some(key)
                && !matches!(
                    job.state,
                    JobState::Completed | JobState::Dlq | JobState::Cancelled
                )
            {
                return Ok(Some(job));
            }
        }
        Ok(None)
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(JOBS_TABLE)?;
        let mut active = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let job: Job = serde_json::from_slice(value.value())?;
            if job.state == JobState::Active {
                active.push(job);
            }
        }
        Ok(active)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use redb::ReadableTableMetadata;
    use serde_json::json;
    use tempfile::NamedTempFile;

    /// Helper: create a temporary RedbStorage instance.
    fn temp_storage() -> RedbStorage {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        // Drop the NamedTempFile so redb can create its own file at the path.
        drop(tmp);
        RedbStorage::new(&path).unwrap()
    }

    /// Helper: create a test job in the given queue.
    fn test_job(queue: &str) -> Job {
        Job::new(queue, "test-job", json!({"key": "value"}))
    }

    // ── Task 2: Insert/Get ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_insert_and_get_job() {
        let storage = temp_storage();
        let job = test_job("emails");

        let id = storage.insert_job(&job).await.unwrap();
        assert_eq!(id, job.id);

        let retrieved = storage.get_job(id).await.unwrap().expect("job should exist");
        assert_eq!(retrieved.id, job.id);
        assert_eq!(retrieved.queue, "emails");
        assert_eq!(retrieved.name, "test-job");
        assert_eq!(retrieved.state, JobState::Waiting);
        assert_eq!(retrieved.data, json!({"key": "value"}));
        assert_eq!(retrieved.priority, 0);
        assert_eq!(retrieved.max_attempts, 3);
    }

    #[tokio::test]
    async fn test_get_nonexistent_job_returns_none() {
        let storage = temp_storage();
        let fake_id = uuid::Uuid::now_v7();
        let result = storage.get_job(fake_id).await.unwrap();
        assert!(result.is_none());
    }

    // ── Task 3: Update/Delete/Dequeue ───────────────────────────────────

    #[tokio::test]
    async fn test_update_job() {
        let storage = temp_storage();
        let mut job = test_job("emails");
        storage.insert_job(&job).await.unwrap();

        // Update state and attempt count.
        job.state = JobState::Active;
        job.attempt = 1;
        job.updated_at = Utc::now();
        storage.update_job(&job).await.unwrap();

        let retrieved = storage.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(retrieved.state, JobState::Active);
        assert_eq!(retrieved.attempt, 1);
    }

    #[tokio::test]
    async fn test_delete_job() {
        let storage = temp_storage();
        let job = test_job("emails");
        let id = storage.insert_job(&job).await.unwrap();

        storage.delete_job(id).await.unwrap();

        let result = storage.get_job(id).await.unwrap();
        assert!(result.is_none(), "deleted job should not be found");
    }

    #[tokio::test]
    async fn test_dequeue_returns_waiting_jobs_fifo() {
        let storage = temp_storage();

        // Insert two jobs with the same priority; earlier created_at should come first.
        let job1 = test_job("emails");
        let mut job2 = test_job("emails");
        // Ensure job2 has a later created_at.
        job2.created_at = job1.created_at + Duration::seconds(1);

        storage.insert_job(&job1).await.unwrap();
        storage.insert_job(&job2).await.unwrap();

        // Dequeue 1 — should get job1 (FIFO).
        let dequeued = storage.dequeue("emails", 1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(dequeued[0].id, job1.id);
        assert_eq!(dequeued[0].state, JobState::Active);
        assert!(dequeued[0].started_at.is_some());

        // Verify job1 is now Active in storage.
        let stored = storage.get_job(job1.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Active);
    }

    #[tokio::test]
    async fn test_dequeue_empty_queue_returns_empty() {
        let storage = temp_storage();
        let dequeued = storage.dequeue("nonexistent", 5).await.unwrap();
        assert!(dequeued.is_empty());
    }

    #[tokio::test]
    async fn test_dequeue_respects_priority() {
        let storage = temp_storage();

        let mut low = test_job("work");
        low.priority = 1;

        let mut high = test_job("work");
        high.priority = 10;
        // Give high a later created_at to prove priority wins over FIFO.
        high.created_at = low.created_at + Duration::seconds(5);

        storage.insert_job(&low).await.unwrap();
        storage.insert_job(&high).await.unwrap();

        let dequeued = storage.dequeue("work", 1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(
            dequeued[0].id, high.id,
            "higher priority job should be dequeued first"
        );
        assert_eq!(dequeued[0].state, JobState::Active);
    }

    // ── Task 4: Counts/DLQ/Schedules/Cleanup ────────────────────────────

    #[tokio::test]
    async fn test_queue_counts() {
        let storage = temp_storage();

        // Insert jobs with various states.
        let mut j_waiting = test_job("q");
        j_waiting.state = JobState::Waiting;

        let mut j_active = test_job("q");
        j_active.state = JobState::Active;

        let mut j_completed = test_job("q");
        j_completed.state = JobState::Completed;

        let mut j_failed = test_job("q");
        j_failed.state = JobState::Failed;

        let mut j_delayed = test_job("q");
        j_delayed.state = JobState::Delayed;

        let mut j_dlq = test_job("q");
        j_dlq.state = JobState::Dlq;

        // A job in a different queue — should not be counted.
        let j_other = test_job("other");

        for job in [
            &j_waiting, &j_active, &j_completed, &j_failed, &j_delayed, &j_dlq, &j_other,
        ] {
            storage.insert_job(job).await.unwrap();
        }

        let counts = storage.get_queue_counts("q").await.unwrap();
        assert_eq!(counts.waiting, 1);
        assert_eq!(counts.active, 1);
        assert_eq!(counts.completed, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.delayed, 1);
        assert_eq!(counts.dlq, 1);
    }

    #[tokio::test]
    async fn test_move_to_dlq_and_retrieve() {
        let storage = temp_storage();

        let mut job = test_job("emails");
        job.state = JobState::Failed;
        storage.insert_job(&job).await.unwrap();

        storage
            .move_to_dlq(&job, "max retries exceeded")
            .await
            .unwrap();

        // Verify the job is now in DLQ state.
        let stored = storage.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Dlq);
        assert_eq!(stored.last_error.as_deref(), Some("max retries exceeded"));

        // Verify get_dlq_jobs returns it.
        let dlq_jobs = storage.get_dlq_jobs("emails", 10).await.unwrap();
        assert_eq!(dlq_jobs.len(), 1);
        assert_eq!(dlq_jobs[0].id, job.id);
    }

    #[tokio::test]
    async fn test_scheduled_jobs_ready() {
        let storage = temp_storage();

        let mut job = test_job("emails");
        job.state = JobState::Delayed;
        // Set delay_until to the past so it should be ready.
        job.delay_until = Some(Utc::now() - Duration::seconds(60));
        storage.insert_job(&job).await.unwrap();

        // Also insert a future-delayed job that should NOT be returned.
        let mut future_job = test_job("emails");
        future_job.state = JobState::Delayed;
        future_job.delay_until = Some(Utc::now() + Duration::hours(1));
        storage.insert_job(&future_job).await.unwrap();

        let ready = storage.get_ready_scheduled(Utc::now()).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, job.id);
    }

    #[tokio::test]
    async fn test_upsert_and_get_schedules() {
        let storage = temp_storage();

        let now = Utc::now();
        let schedule = Schedule {
            name: "daily-report".to_string(),
            queue: "reports".to_string(),
            job_name: "generate-report".to_string(),
            job_data: json!({"type": "daily"}),
            cron_expr: Some("0 0 * * *".to_string()),
            every_ms: None,
            timezone: None,
            max_executions: None,
            execution_count: 0,
            paused: false,
            last_run_at: None,
            next_run_at: None,
            created_at: now,
            updated_at: now,
        };

        storage.upsert_schedule(&schedule).await.unwrap();

        let active = storage.get_active_schedules().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "daily-report");
        assert_eq!(active[0].queue, "reports");

        // Upsert again with paused=true — should not appear in active schedules.
        let mut paused = schedule.clone();
        paused.paused = true;
        storage.upsert_schedule(&paused).await.unwrap();

        let active = storage.get_active_schedules().await.unwrap();
        assert!(active.is_empty(), "paused schedule should not be active");

        // Delete the schedule.
        storage.delete_schedule("daily-report").await.unwrap();

        // Even if we get all (including paused), there should be none.
        let read_txn = storage.db.begin_read().unwrap();
        let table = read_txn.open_table(SCHEDULES_TABLE).unwrap();
        assert_eq!(table.len().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_remove_completed_before() {
        let storage = temp_storage();

        // Insert an old completed job.
        let mut old_job = test_job("cleanup");
        old_job.state = JobState::Completed;
        old_job.completed_at = Some(Utc::now() - Duration::days(30));
        storage.insert_job(&old_job).await.unwrap();

        // Insert a recent completed job that should NOT be removed.
        let mut recent_job = test_job("cleanup");
        recent_job.state = JobState::Completed;
        recent_job.completed_at = Some(Utc::now());
        storage.insert_job(&recent_job).await.unwrap();

        // Insert a waiting job — should not be touched.
        let waiting_job = test_job("cleanup");
        storage.insert_job(&waiting_job).await.unwrap();

        let cutoff = Utc::now() - Duration::days(7);
        let removed = storage.remove_completed_before(cutoff).await.unwrap();
        assert_eq!(removed, 1);

        // Old job is gone.
        assert!(storage.get_job(old_job.id).await.unwrap().is_none());
        // Recent completed job is still there.
        assert!(storage.get_job(recent_job.id).await.unwrap().is_some());
        // Waiting job untouched.
        assert!(storage.get_job(waiting_job.id).await.unwrap().is_some());
    }
}
