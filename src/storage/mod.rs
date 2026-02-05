pub mod redb;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::engine::models::{Job, JobId, QueueCounts, Schedule};

pub use self::redb::RedbStorage;

/// Trait abstracting the storage layer, allowing multiple backend implementations.
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    // Job operations
    async fn insert_job(&self, job: &Job) -> anyhow::Result<JobId>;
    async fn get_job(&self, id: JobId) -> anyhow::Result<Option<Job>>;
    async fn update_job(&self, job: &Job) -> anyhow::Result<()>;
    async fn delete_job(&self, id: JobId) -> anyhow::Result<()>;

    // Queue operations
    async fn dequeue(&self, queue: &str, count: u32) -> anyhow::Result<Vec<Job>>;
    async fn get_queue_counts(&self, queue: &str) -> anyhow::Result<QueueCounts>;

    // Scheduled jobs
    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Job>>;

    // DLQ
    async fn move_to_dlq(&self, job: &Job, reason: &str) -> anyhow::Result<()>;
    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> anyhow::Result<Vec<Job>>;

    // Cleanup
    async fn remove_completed_before(&self, before: DateTime<Utc>) -> anyhow::Result<u64>;

    // Cron schedules
    async fn upsert_schedule(&self, schedule: &Schedule) -> anyhow::Result<()>;
    async fn get_active_schedules(&self) -> anyhow::Result<Vec<Schedule>>;
    async fn delete_schedule(&self, name: &str) -> anyhow::Result<()>;
}
