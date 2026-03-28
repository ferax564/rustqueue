//! RustQueue — A high-performance distributed job scheduler.
//!
//! RustQueue is a zero-dependency, single-binary job queue and task scheduler
//! that can be used as a standalone server or embedded as a library.

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
