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
use crate::engine::queue::{FailResult, JobOptions, QueueInfo, QueueManager};
use crate::storage::{MemoryStorage, RedbStorage, StorageBackend};

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
}

impl RustQueue {
    /// Create a builder backed by an in-memory storage (no persistence).
    ///
    /// Data is lost when the `RustQueue` instance is dropped.
    /// Ideal for tests and short-lived processes.
    pub fn memory() -> RustQueueBuilder {
        RustQueueBuilder {
            storage: Arc::new(MemoryStorage::new()),
        }
    }

    /// Create a builder backed by a redb file at the given path.
    ///
    /// The database file is created if it does not already exist.
    /// Data survives process restarts.
    pub fn redb(path: impl AsRef<Path>) -> anyhow::Result<RustQueueBuilder> {
        let storage = Arc::new(RedbStorage::new(path)?);
        Ok(RustQueueBuilder { storage })
    }
}

impl RustQueueBuilder {
    /// Build the [`RustQueue`] instance.
    pub fn build(self) -> anyhow::Result<RustQueue> {
        Ok(RustQueue {
            manager: QueueManager::new(self.storage),
        })
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
