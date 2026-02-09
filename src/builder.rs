//! High-level builder for creating a [`RustQueue`] instance as an embedded library.
//!
//! This module provides a zero-config entry point for using RustQueue without
//! running a server. Just pick a storage backend and call `.build()`:
//!
//! ```no_run
//! use rustqueue::RustQueue;
//! use serde_json::json;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let rq = RustQueue::memory().build()?;
//! let id = rq.push("emails", "send-welcome", json!({"to": "a@b.com"}), None).await?;
//! # Ok(())
//! # }
//! ```

use std::path::Path;
use std::sync::Arc;

use crate::engine::error::RustQueueError;
use crate::engine::models::{Job, JobId, QueueCounts};
use crate::engine::plugins::WorkerRegistry;
use crate::engine::queue::{FailResult, JobOptions, QueueInfo, QueueManager};
use crate::storage::{
    BufferedRedbConfig, BufferedRedbStorage, HybridConfig, HybridStorage, MemoryStorage,
    RedbStorage, StorageBackend,
};

/// High-level client for RustQueue, usable as an embedded library.
///
/// Create via [`RustQueue::memory()`] for an ephemeral in-memory backend or
/// [`RustQueue::redb(path)`] for a persistent file-backed backend.
///
/// All methods delegate to an internal [`QueueManager`].
pub struct RustQueue {
    manager: QueueManager,
}

/// Builder for constructing a [`RustQueue`] instance.
///
/// Obtained via [`RustQueue::memory()`] or [`RustQueue::redb(path)`].
/// Call [`.build()`](RustQueueBuilder::build) to finalise.
pub struct RustQueueBuilder {
    storage: Arc<dyn StorageBackend>,
    buffered_config: Option<BufferedRedbConfig>,
    hybrid_config: Option<HybridConfig>,
    worker_registry: Option<Arc<WorkerRegistry>>,
}

impl RustQueue {
    /// Create a builder backed by an in-memory storage (no persistence).
    ///
    /// Data is lost when the `RustQueue` instance is dropped.
    /// Ideal for tests and short-lived processes.
    pub fn memory() -> RustQueueBuilder {
        RustQueueBuilder {
            storage: Arc::new(MemoryStorage::new()),
            buffered_config: None,
            hybrid_config: None,
            worker_registry: None,
        }
    }

    /// Create a builder backed by a redb file at the given path.
    ///
    /// The database file is created if it does not already exist.
    /// Data survives process restarts.
    pub fn redb(path: impl AsRef<Path>) -> anyhow::Result<RustQueueBuilder> {
        let storage = Arc::new(RedbStorage::new(path)?);
        Ok(RustQueueBuilder {
            storage,
            buffered_config: None,
            hybrid_config: None,
            worker_registry: None,
        })
    }

    /// Create a builder backed by hybrid memory+disk storage.
    ///
    /// All reads/writes serve from in-memory DashMap. A background task
    /// snapshots dirty entries to a redb file periodically.
    /// Up to `snapshot_interval` of data may be lost on crash.
    pub fn hybrid(path: impl AsRef<Path>) -> anyhow::Result<RustQueueBuilder> {
        let storage = Arc::new(RedbStorage::new(path)?);
        Ok(RustQueueBuilder {
            storage,
            buffered_config: None,
            hybrid_config: Some(HybridConfig::default()),
            worker_registry: None,
        })
    }
}

impl RustQueueBuilder {
    /// Enable write coalescing with the given configuration.
    ///
    /// When enabled, single `insert_job` and `complete_job` calls are buffered
    /// and flushed as batches for significantly higher throughput.
    pub fn with_write_coalescing(mut self, config: BufferedRedbConfig) -> Self {
        self.buffered_config = Some(config);
        self
    }

    /// Configure the hybrid storage snapshot settings.
    ///
    /// Only effective when using [`RustQueue::hybrid()`].
    pub fn with_hybrid_config(mut self, config: HybridConfig) -> Self {
        self.hybrid_config = Some(config);
        self
    }

    /// Attach a worker plugin registry used for engine-specific dispatch.
    pub fn with_worker_registry(mut self, registry: Arc<WorkerRegistry>) -> Self {
        self.worker_registry = Some(registry);
        self
    }

    /// Build the [`RustQueue`] instance.
    pub fn build(self) -> anyhow::Result<RustQueue> {
        let storage: Arc<dyn StorageBackend> = if let Some(config) = self.hybrid_config {
            Arc::new(HybridStorage::new(self.storage, config))
        } else if let Some(config) = self.buffered_config {
            Arc::new(BufferedRedbStorage::new(self.storage, config))
        } else {
            self.storage
        };
        let manager = if let Some(registry) = self.worker_registry {
            QueueManager::new(storage).with_worker_registry(registry)
        } else {
            QueueManager::new(storage)
        };
        Ok(RustQueue { manager })
    }
}

// ── Delegated operations ─────────────────────────────────────────────────────

impl RustQueue {
    /// Enqueue a new job, optionally applying [`JobOptions`].
    ///
    /// Returns the generated [`JobId`].
    pub async fn push(
        &self,
        queue: &str,
        name: &str,
        data: serde_json::Value,
        opts: Option<JobOptions>,
    ) -> Result<JobId, RustQueueError> {
        self.manager.push(queue, name, data, opts).await
    }

    /// Dequeue up to `count` jobs from the given queue.
    ///
    /// Returned jobs are transitioned to `Active` state atomically.
    pub async fn pull(&self, queue: &str, count: u32) -> Result<Vec<Job>, RustQueueError> {
        self.manager.pull(queue, count).await
    }

    /// Pull one job and dispatch it through the registered worker plugin.
    ///
    /// Returns the processed job id, or `None` if no job was available.
    pub async fn dispatch_next_with_registered_worker(
        &self,
        queue: &str,
    ) -> Result<Option<JobId>, RustQueueError> {
        self.manager
            .dispatch_next_with_registered_worker(queue)
            .await
    }

    /// Acknowledge successful completion of a job.
    ///
    /// The job must be in `Active` state; otherwise an error is returned.
    pub async fn ack(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<(), RustQueueError> {
        self.manager.ack(id, result).await
    }

    /// Report a job failure. The engine decides whether to retry or move to DLQ.
    pub async fn fail(&self, id: JobId, error: &str) -> Result<FailResult, RustQueueError> {
        self.manager.fail(id, error).await
    }

    /// Cancel a job that is still waiting or delayed.
    pub async fn cancel(&self, id: JobId) -> Result<(), RustQueueError> {
        self.manager.cancel(id).await
    }

    /// Retrieve a single job by ID, or `None` if it does not exist.
    pub async fn get_job(&self, id: JobId) -> Result<Option<Job>, RustQueueError> {
        self.manager.get_job(id).await
    }

    /// List all known queues together with their current counts.
    pub async fn list_queues(&self) -> Result<Vec<QueueInfo>, RustQueueError> {
        self.manager.list_queues().await
    }

    /// Get counts for a specific queue.
    pub async fn get_queue_stats(&self, queue: &str) -> Result<QueueCounts, RustQueueError> {
        self.manager.get_queue_stats(queue).await
    }

    /// Update progress on an active job, optionally appending a log message.
    ///
    /// Progress is clamped to 0-100. The job must be in `Active` state.
    pub async fn update_progress(
        &self,
        id: JobId,
        progress: u8,
        message: Option<String>,
    ) -> Result<(), RustQueueError> {
        self.manager.update_progress(id, progress, message).await
    }
}
