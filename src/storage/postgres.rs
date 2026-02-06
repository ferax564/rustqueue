//! PostgreSQL storage backend powered by [sqlx](https://docs.rs/sqlx).
//!
//! Uses a hybrid schema: the full `Job`/`Schedule` is stored as JSONB in a `data`
//! column, while key fields (queue, state, priority, created_at) are extracted
//! into dedicated columns for efficient indexing.
//!
//! The `dequeue` method uses `SELECT ... FOR UPDATE SKIP LOCKED` for
//! concurrent-safe dequeueing -- multiple workers can pull from the same
//! queue without contention.
//!
//! Enabled via the `postgres` feature flag:
//! ```toml
//! rustqueue = { version = "0.1", features = ["postgres"] }
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::StorageBackend;

/// PostgreSQL storage backend -- persists jobs and schedules in a PostgreSQL database.
///
/// All operations use native async queries via `sqlx::PgPool`. No `Mutex` is needed
/// because the pool handles connection management and Postgres itself provides
/// transactional isolation (e.g. `FOR UPDATE SKIP LOCKED` for dequeue).
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Connect to PostgreSQL and run schema migrations.
    ///
    /// The `database_url` should be a standard PostgreSQL connection string,
    /// e.g. `postgres://user:password@localhost:5432/rustqueue`.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .context("failed to connect to PostgreSQL")?;

        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Create a `PostgresStorage` by blocking on the async constructor.
    ///
    /// Only for use in contexts where a tokio runtime is already running
    /// (e.g. inside `#[tokio::test]` or when called from synchronous test
    /// harness code).
    pub fn new_blocking(database_url: &str) -> Result<Self> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(Self::new(database_url))
        })
    }

    /// Return a reference to the underlying connection pool.
    ///
    /// Useful for direct SQL access (e.g. in advanced tooling or extensions).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Delete all rows from the `jobs` and `schedules` tables.
    ///
    /// Intended for test cleanup -- call this before each test to ensure
    /// isolation.
    pub async fn clear_all(&self) -> Result<()> {
        sqlx::query("DELETE FROM jobs")
            .execute(&self.pool)
            .await
            .context("failed to clear jobs table")?;
        sqlx::query("DELETE FROM schedules")
            .execute(&self.pool)
            .await
            .context("failed to clear schedules table")?;
        Ok(())
    }

    /// Create tables and indexes if they don't already exist.
    async fn run_migrations(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id UUID PRIMARY KEY,
                queue TEXT NOT NULL,
                state TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMPTZ NOT NULL,
                data JSONB NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .context("failed to create jobs table")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_jobs_dequeue \
             ON jobs (queue, state, priority DESC, created_at ASC)",
        )
        .execute(pool)
        .await
        .context("failed to create dequeue index")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs (state)")
            .execute(pool)
            .await
            .context("failed to create state index")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS schedules (
                name TEXT PRIMARY KEY,
                data JSONB NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .context("failed to create schedules table")?;

        Ok(())
    }

    /// Serialize a `JobState` to the lowercase snake_case string used in the
    /// `state` column.
    fn state_str(state: &JobState) -> String {
        // serde serializes JobState variants as lowercase snake_case strings
        // (e.g. "waiting", "active", "dlq"). We reuse that for the column value.
        let v = serde_json::to_value(state).unwrap_or(serde_json::Value::Null);
        v.as_str().unwrap_or("unknown").to_string()
    }
}

#[async_trait]
impl StorageBackend for PostgresStorage {
    // -- Job CRUD -------------------------------------------------------------

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let data = serde_json::to_value(job).context("failed to serialize job")?;
        let state_str = Self::state_str(&job.state);

        sqlx::query(
            "INSERT INTO jobs (id, queue, state, priority, created_at, data) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(job.id)
        .bind(&job.queue)
        .bind(&state_str)
        .bind(job.priority)
        .bind(job.created_at)
        .bind(&data)
        .execute(&self.pool)
        .await
        .context("failed to insert job")?;

        Ok(job.id)
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT data FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("failed to fetch job")?;

        match row {
            Some((data,)) => {
                let job: Job = serde_json::from_value(data)
                    .context("failed to deserialize job from JSONB")?;
                Ok(Some(job))
            }
            None => Ok(None),
        }
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        let data = serde_json::to_value(job).context("failed to serialize job")?;
        let state_str = Self::state_str(&job.state);

        sqlx::query(
            "UPDATE jobs SET queue = $2, state = $3, priority = $4, \
             created_at = $5, data = $6 WHERE id = $1",
        )
        .bind(job.id)
        .bind(&job.queue)
        .bind(&state_str)
        .bind(job.priority)
        .bind(job.created_at)
        .bind(&data)
        .execute(&self.pool)
        .await
        .context("failed to update job")?;

        Ok(())
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        sqlx::query("DELETE FROM jobs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete job")?;

        Ok(())
    }

    // -- Queue operations -----------------------------------------------------

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        let mut tx = self.pool.begin().await.context("failed to begin transaction")?;

        // Select and lock waiting jobs -- `FOR UPDATE SKIP LOCKED` ensures
        // concurrent workers do not contend on the same rows.
        let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
            "SELECT data FROM jobs \
             WHERE queue = $1 AND state = 'waiting' \
             ORDER BY priority DESC, created_at ASC \
             LIMIT $2 \
             FOR UPDATE SKIP LOCKED",
        )
        .bind(queue)
        .bind(count as i32)
        .fetch_all(&mut *tx)
        .await
        .context("failed to select jobs for dequeue")?;

        let now = Utc::now();
        let mut result = Vec::with_capacity(rows.len());

        for (data,) in rows {
            let mut job: Job =
                serde_json::from_value(data).context("failed to deserialize queued job")?;

            job.state = JobState::Active;
            job.started_at = Some(now);
            job.updated_at = now;

            let job_data = serde_json::to_value(&job)?;
            let state_str = Self::state_str(&job.state);

            sqlx::query("UPDATE jobs SET state = $2, data = $3 WHERE id = $1")
                .bind(job.id)
                .bind(&state_str)
                .bind(&job_data)
                .execute(&mut *tx)
                .await
                .context("failed to update dequeued job")?;

            result.push(job);
        }

        tx.commit().await.context("failed to commit dequeue transaction")?;
        Ok(result)
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT state, COUNT(*) as cnt FROM jobs WHERE queue = $1 GROUP BY state",
        )
        .bind(queue)
        .fetch_all(&self.pool)
        .await
        .context("failed to get queue counts")?;

        let mut counts = QueueCounts::default();

        for (state, cnt) in rows {
            let cnt = cnt as u64;
            match state.as_str() {
                "waiting" | "created" => counts.waiting += cnt,
                "active" => counts.active = cnt,
                "delayed" => counts.delayed = cnt,
                "completed" => counts.completed = cnt,
                "failed" => counts.failed = cnt,
                "dlq" => counts.dlq = cnt,
                // Cancelled, Blocked are not tracked in QueueCounts for v0.1.
                _ => {}
            }
        }

        Ok(counts)
    }

    // -- Scheduled jobs -------------------------------------------------------

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        // Fetch all delayed jobs that have a delay_until value.
        // We filter in Rust to avoid timestamp format edge cases across
        // different serde serialization formats.
        let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
            "SELECT data FROM jobs \
             WHERE state = 'delayed' \
             AND data->>'delay_until' IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch delayed jobs")?;

        let mut ready = Vec::new();

        for (data,) in rows {
            let job: Job = serde_json::from_value(data)?;
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
        let mut updated = job.clone();
        updated.state = JobState::Dlq;
        updated.last_error = Some(reason.to_string());
        updated.updated_at = Utc::now();

        let data = serde_json::to_value(&updated)?;
        let state_str = Self::state_str(&updated.state);

        sqlx::query(
            "UPDATE jobs SET state = $2, data = $3 WHERE id = $1",
        )
        .bind(updated.id)
        .bind(&state_str)
        .bind(&data)
        .execute(&self.pool)
        .await
        .context("failed to move job to DLQ")?;

        Ok(())
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
            "SELECT data FROM jobs WHERE queue = $1 AND state = 'dlq' LIMIT $2",
        )
        .bind(queue)
        .bind(limit as i32)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch DLQ jobs")?;

        let mut jobs = Vec::with_capacity(rows.len());
        for (data,) in rows {
            jobs.push(serde_json::from_value(data)?);
        }

        Ok(jobs)
    }

    // -- Cleanup --------------------------------------------------------------

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        // Fetch completed jobs, filter by completed_at in Rust (safe timestamp
        // comparison), then delete matching rows.
        let rows: Vec<(uuid::Uuid, serde_json::Value)> = sqlx::query_as(
            "SELECT id, data FROM jobs \
             WHERE state = 'completed' \
             AND data->>'completed_at' IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch completed jobs for cleanup")?;

        let mut to_remove: Vec<uuid::Uuid> = Vec::new();

        for (id, data) in rows {
            let job: Job = serde_json::from_value(data)?;
            if let Some(completed_at) = job.completed_at {
                if completed_at < before {
                    to_remove.push(id);
                }
            }
        }

        if to_remove.is_empty() {
            return Ok(0);
        }

        let count = to_remove.len() as u64;

        for id in &to_remove {
            sqlx::query("DELETE FROM jobs WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }

        Ok(count)
    }

    // -- Cron schedules -------------------------------------------------------

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        let data =
            serde_json::to_value(schedule).context("failed to serialize schedule")?;

        sqlx::query(
            "INSERT INTO schedules (name, data) VALUES ($1, $2) \
             ON CONFLICT (name) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(&schedule.name)
        .bind(&data)
        .execute(&self.pool)
        .await
        .context("failed to upsert schedule")?;

        Ok(())
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        let rows: Vec<(serde_json::Value,)> =
            sqlx::query_as("SELECT data FROM schedules")
                .fetch_all(&self.pool)
                .await
                .context("failed to fetch schedules")?;

        let mut schedules = Vec::new();
        for (data,) in rows {
            let schedule: Schedule = serde_json::from_value(data)?;
            if !schedule.paused {
                schedules.push(schedule);
            }
        }

        Ok(schedules)
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM schedules WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .context("failed to delete schedule")?;

        Ok(())
    }

    // -- Discovery ------------------------------------------------------------

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT queue FROM jobs ORDER BY queue")
                .fetch_all(&self.pool)
                .await
                .context("failed to list queue names")?;

        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT data FROM jobs \
             WHERE queue = $1 \
             AND data->>'unique_key' = $2 \
             AND state NOT IN ('completed', 'dlq', 'cancelled') \
             LIMIT 1",
        )
        .bind(queue)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to look up job by unique key")?;

        match row {
            Some((data,)) => Ok(Some(serde_json::from_value(data)?)),
            None => Ok(None),
        }
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        let rows: Vec<(serde_json::Value,)> =
            sqlx::query_as("SELECT data FROM jobs WHERE state = 'active'")
                .fetch_all(&self.pool)
                .await
                .context("failed to fetch active jobs")?;

        let mut jobs = Vec::with_capacity(rows.len());
        for (data,) in rows {
            jobs.push(serde_json::from_value(data)?);
        }

        Ok(jobs)
    }
}
