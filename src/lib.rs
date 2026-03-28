//! # RustQueue — Background jobs without infrastructure
//!
//! Add persistent, crash-safe background job processing to any Rust application.
//! No Redis. No RabbitMQ. No external services.
//!
//! ```no_run
//! use rustqueue::RustQueue;
//! use serde_json::json;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let rq = RustQueue::redb("./jobs.db")?.build()?;
//! let id = rq.push("emails", "send-welcome", json!({"to": "a@b.com"}), None).await?;
//! let jobs = rq.pull("emails", 1).await?;
//! rq.ack(jobs[0].id, None).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Jobs persist to an embedded ACID database. They survive crashes and restarts.
//! When you outgrow embedded mode, run the same engine as a standalone server
//! with `rustqueue serve`.

pub mod api;
pub mod auth;
pub mod builder;
pub mod config;
pub mod dashboard;
pub mod engine;
pub mod metrics_registry;
pub mod protocol;
pub mod storage;
pub mod axum_integration;

// Re-export core types for library consumers
pub use builder::RustQueue;
pub use engine::error::RustQueueError;
pub use engine::models::{
    BackoffStrategy, Job, JobId, JobState, QueueCounts, QueueOrdering, Schedule,
};
pub use engine::plugins::{JobProcessor, WorkerFactory, WorkerRegistry};
pub use engine::queue::{FailResult, JobOptions, QueueInfo, QueueManager};
pub use engine::workflow::{CollectArrayJoin, Workflow, WorkflowJoin, WorkflowStep};
pub use metrics_registry::MetricsRegistry;
pub use storage::StorageBackend;
