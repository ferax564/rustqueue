//! RustQueue — A high-performance distributed job scheduler.
//!
//! RustQueue is a zero-dependency, single-binary job queue and task scheduler
//! that can be used as a standalone server or embedded as a library.

pub mod api;
pub mod builder;
pub mod config;
pub mod engine;
pub mod protocol;
pub mod storage;

// Re-export core types for library consumers
pub use builder::RustQueue;
pub use engine::error::RustQueueError;
pub use engine::models::{BackoffStrategy, Job, JobId, JobState, QueueCounts, QueueOrdering};
pub use engine::queue::{FailResult, JobOptions, QueueInfo, QueueManager};
pub use storage::StorageBackend;
