//! SQLite storage backend powered by [rusqlite](https://docs.rs/rusqlite).
//!
//! Stores jobs and schedules as JSON TEXT in two tables:
//! - `jobs` (id BLOB PK, data TEXT)
//! - `schedules` (name TEXT PK, data TEXT)
//!
//! Uses WAL journal mode for concurrent reads. The `dequeue` method uses
//! `json_extract()` for filtering and ordering directly in SQL.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::StorageBackend;

/// SQLite storage backend -- persists jobs and schedules in a local SQLite database.
///
/// All operations acquire a `Mutex<Connection>` lock. Since rusqlite's `Connection`
/// is synchronous, the async trait methods call it directly without `spawn_blocking`
/// (same pattern as the redb backend for v0.1).
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    /// Create or open a SQLite database at the given path.
    ///
    /// On first open the required tables are created automatically.
    /// WAL journal mode is enabled for concurrent read performance.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("failed to open SQLite at {:?}", path.as_ref()))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS jobs (
                id BLOB PRIMARY KEY,
                data TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS schedules (
                name TEXT PRIMARY KEY,
                data TEXT NOT NULL
            );
        ",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[async_trait]
impl StorageBackend for SqliteStorage {
    // -- Job CRUD -------------------------------------------------------------

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let conn = self.conn.lock().unwrap();
        let id = job.id;
        let key = id.as_bytes().to_vec();
        let data = serde_json::to_string(job).context("failed to serialize job")?;

        conn.execute(
            "INSERT OR REPLACE INTO jobs (id, data) VALUES (?1, ?2)",
            (&key, &data),
        )?;

        Ok(id)
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        let conn = self.conn.lock().unwrap();
        let key = id.as_bytes().to_vec();

        let mut stmt = conn.prepare("SELECT data FROM jobs WHERE id = ?1")?;
        let result = stmt.query_row((&key,), |row| {
            let data: String = row.get(0)?;
            Ok(data)
        });

        match result {
            Ok(data) => Ok(Some(serde_json::from_str(&data)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let key = job.id.as_bytes().to_vec();
        let data = serde_json::to_string(job).context("failed to serialize job")?;

        conn.execute(
            "INSERT OR REPLACE INTO jobs (id, data) VALUES (?1, ?2)",
            (&key, &data),
        )?;

        Ok(())
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let key = id.as_bytes().to_vec();

        conn.execute("DELETE FROM jobs WHERE id = ?1", (&key,))?;

        Ok(())
    }

    // -- Queue operations -----------------------------------------------------

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        let conn = self.conn.lock().unwrap();

        // Use a transaction for atomicity: select candidates, update their state,
        // and return them all within a single commit.
        let tx = conn.unchecked_transaction()?;

        // Collect candidate rows in a scoped block so the prepared statement
        // (which borrows `tx`) is dropped before we call `tx.commit()`.
        let rows: Vec<(Vec<u8>, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, data FROM jobs
                 WHERE json_extract(data, '$.queue') = ?1
                   AND json_extract(data, '$.state') = 'waiting'
                 ORDER BY json_extract(data, '$.priority') DESC,
                          json_extract(data, '$.created_at') ASC
                 LIMIT ?2",
            )?;

            stmt.query_map((queue, count), |row| {
                let id: Vec<u8> = row.get(0)?;
                let data: String = row.get(1)?;
                Ok((id, data))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let now = Utc::now();
        let mut selected = Vec::with_capacity(rows.len());

        for (key, data) in rows {
            let mut job: Job = serde_json::from_str(&data)?;
            job.state = JobState::Active;
            job.started_at = Some(now);
            job.updated_at = now;

            let updated_data = serde_json::to_string(&job)?;
            tx.execute(
                "UPDATE jobs SET data = ?1 WHERE id = ?2",
                (&updated_data, &key),
            )?;

            selected.push(job);
        }

        tx.commit()?;
        Ok(selected)
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT data FROM jobs
             WHERE json_extract(data, '$.queue') = ?1",
        )?;

        let mut counts = QueueCounts::default();
        let mut rows = stmt.query((queue,))?;

        while let Some(row) = rows.next()? {
            let data: String = row.get(0)?;
            let job: Job = serde_json::from_str(&data)?;

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

    // -- Scheduled jobs -------------------------------------------------------

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT data FROM jobs
             WHERE json_extract(data, '$.state') = 'delayed'
               AND json_extract(data, '$.delay_until') IS NOT NULL",
        )?;

        let mut ready = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let data: String = row.get(0)?;
            let job: Job = serde_json::from_str(&data)?;
            if let Some(delay_until) = job.delay_until {
                if delay_until <= now {
                    ready.push(job);
                }
            }
        }

        Ok(ready)
    }

    // -- DLQ ------------------------------------------------------------------

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        let mut updated = job.clone();
        updated.state = JobState::Dlq;
        updated.last_error = Some(reason.to_string());
        updated.updated_at = Utc::now();

        let key = updated.id.as_bytes().to_vec();
        let data = serde_json::to_string(&updated)?;

        conn.execute(
            "INSERT OR REPLACE INTO jobs (id, data) VALUES (?1, ?2)",
            (&key, &data),
        )?;

        Ok(())
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT data FROM jobs
             WHERE json_extract(data, '$.queue') = ?1
               AND json_extract(data, '$.state') = 'dlq'
             LIMIT ?2",
        )?;

        let mut dlq_jobs = Vec::new();
        let mut rows = stmt.query((queue, limit))?;

        while let Some(row) = rows.next()? {
            let data: String = row.get(0)?;
            let job: Job = serde_json::from_str(&data)?;
            dlq_jobs.push(job);
        }

        Ok(dlq_jobs)
    }

    // -- Cleanup --------------------------------------------------------------

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn.lock().unwrap();

        // Find all completed jobs with completed_at before the cutoff.
        // We need to parse the timestamp from JSON and compare it in Rust
        // because SQLite string comparison of ISO 8601 timestamps is unreliable
        // with timezone offsets.
        let mut stmt = conn.prepare(
            "SELECT id, data FROM jobs
             WHERE json_extract(data, '$.state') = 'completed'
               AND json_extract(data, '$.completed_at') IS NOT NULL",
        )?;

        let mut to_remove: Vec<Vec<u8>> = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let key: Vec<u8> = row.get(0)?;
            let data: String = row.get(1)?;
            let job: Job = serde_json::from_str(&data)?;

            if let Some(completed_at) = job.completed_at {
                if completed_at < before {
                    to_remove.push(key);
                }
            }
        }

        let count = to_remove.len() as u64;
        for key in &to_remove {
            conn.execute("DELETE FROM jobs WHERE id = ?1", (key,))?;
        }

        Ok(count)
    }

    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT id, data FROM jobs
             WHERE json_extract(data, '$.state') = 'failed'",
        )?;

        let mut to_remove: Vec<Vec<u8>> = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let key: Vec<u8> = row.get(0)?;
            let data: String = row.get(1)?;
            let job: Job = serde_json::from_str(&data)?;

            if job.updated_at < before {
                to_remove.push(key);
            }
        }

        let count = to_remove.len() as u64;
        for key in &to_remove {
            conn.execute("DELETE FROM jobs WHERE id = ?1", (key,))?;
        }

        Ok(count)
    }

    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT id, data FROM jobs
             WHERE json_extract(data, '$.state') = 'dlq'",
        )?;

        let mut to_remove: Vec<Vec<u8>> = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let key: Vec<u8> = row.get(0)?;
            let data: String = row.get(1)?;
            let job: Job = serde_json::from_str(&data)?;

            if job.updated_at < before {
                to_remove.push(key);
            }
        }

        let count = to_remove.len() as u64;
        for key in &to_remove {
            conn.execute("DELETE FROM jobs WHERE id = ?1", (key,))?;
        }

        Ok(count)
    }

    // -- Cron schedules -------------------------------------------------------

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let key = &schedule.name;
        let data = serde_json::to_string(schedule).context("failed to serialize schedule")?;

        conn.execute(
            "INSERT OR REPLACE INTO schedules (name, data) VALUES (?1, ?2)",
            (key, &data),
        )?;

        Ok(())
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare("SELECT data FROM schedules")?;
        let mut schedules = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let data: String = row.get(0)?;
            let schedule: Schedule = serde_json::from_str(&data)?;
            if !schedule.paused {
                schedules.push(schedule);
            }
        }

        Ok(schedules)
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM schedules WHERE name = ?1", (name,))?;
        Ok(())
    }

    // -- Discovery ------------------------------------------------------------

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT DISTINCT json_extract(data, '$.queue') FROM jobs",
        )?;

        let mut names = std::collections::BTreeSet::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let queue: String = row.get(0)?;
            names.insert(queue);
        }

        Ok(names.into_iter().collect())
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT data FROM jobs
             WHERE json_extract(data, '$.queue') = ?1
               AND json_extract(data, '$.unique_key') = ?2
               AND json_extract(data, '$.state') NOT IN ('completed', 'dlq', 'cancelled')",
        )?;

        let result = stmt.query_row((queue, key), |row| {
            let data: String = row.get(0)?;
            Ok(data)
        });

        match result {
            Ok(data) => Ok(Some(serde_json::from_str(&data)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT data FROM jobs WHERE json_extract(data, '$.state') = 'active'",
        )?;

        let mut active = Vec::new();
        let mut rows = stmt.query(())?;

        while let Some(row) = rows.next()? {
            let data: String = row.get(0)?;
            let job: Job = serde_json::from_str(&data)?;
            active.push(job);
        }

        Ok(active)
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;
    use tempfile::NamedTempFile;

    /// Helper: create a temporary SqliteStorage instance.
    fn temp_storage() -> SqliteStorage {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        // Drop the NamedTempFile so SQLite can create its own file at the path.
        drop(tmp);
        SqliteStorage::new(&path).unwrap()
    }

    /// Helper: create a test job in the given queue.
    fn test_job(queue: &str) -> Job {
        Job::new(queue, "test-job", json!({"key": "value"}))
    }

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
    }

    #[tokio::test]
    async fn test_get_nonexistent_job_returns_none() {
        let storage = temp_storage();
        let fake_id = uuid::Uuid::now_v7();
        let result = storage.get_job(fake_id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_dequeue_returns_waiting_jobs_fifo() {
        let storage = temp_storage();

        let job1 = test_job("emails");
        let mut job2 = test_job("emails");
        job2.created_at = job1.created_at + Duration::seconds(1);

        storage.insert_job(&job1).await.unwrap();
        storage.insert_job(&job2).await.unwrap();

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
    async fn test_dequeue_respects_priority() {
        let storage = temp_storage();

        let mut low = test_job("work");
        low.priority = 1;

        let mut high = test_job("work");
        high.priority = 10;
        high.created_at = low.created_at + Duration::seconds(5);

        storage.insert_job(&low).await.unwrap();
        storage.insert_job(&high).await.unwrap();

        let dequeued = storage.dequeue("work", 1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(dequeued[0].id, high.id);
        assert_eq!(dequeued[0].state, JobState::Active);
    }

    #[tokio::test]
    async fn test_dequeue_empty_queue_returns_empty() {
        let storage = temp_storage();
        let dequeued = storage.dequeue("nonexistent", 5).await.unwrap();
        assert!(dequeued.is_empty());
    }

    #[tokio::test]
    async fn test_queue_counts() {
        let storage = temp_storage();

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

        let stored = storage.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Dlq);
        assert_eq!(stored.last_error.as_deref(), Some("max retries exceeded"));

        let dlq_jobs = storage.get_dlq_jobs("emails", 10).await.unwrap();
        assert_eq!(dlq_jobs.len(), 1);
        assert_eq!(dlq_jobs[0].id, job.id);
    }

    #[tokio::test]
    async fn test_remove_completed_before() {
        let storage = temp_storage();

        let mut old_job = test_job("cleanup");
        old_job.state = JobState::Completed;
        old_job.completed_at = Some(Utc::now() - Duration::days(30));
        storage.insert_job(&old_job).await.unwrap();

        let mut recent_job = test_job("cleanup");
        recent_job.state = JobState::Completed;
        recent_job.completed_at = Some(Utc::now());
        storage.insert_job(&recent_job).await.unwrap();

        let waiting_job = test_job("cleanup");
        storage.insert_job(&waiting_job).await.unwrap();

        let cutoff = Utc::now() - Duration::days(7);
        let removed = storage.remove_completed_before(cutoff).await.unwrap();
        assert_eq!(removed, 1);

        assert!(storage.get_job(old_job.id).await.unwrap().is_none());
        assert!(storage.get_job(recent_job.id).await.unwrap().is_some());
        assert!(storage.get_job(waiting_job.id).await.unwrap().is_some());
    }
}
