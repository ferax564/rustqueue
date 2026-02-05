//! RustQueue — A high-performance distributed job scheduler.
//!
//! RustQueue is a zero-dependency, single-binary job queue and task scheduler
//! that can be used as a standalone server or embedded as a library.

pub mod config;
pub mod engine;
pub mod storage;

// Re-export core types for library consumers
pub use engine::models::{BackoffStrategy, Job, JobId, JobState, QueueOrdering};
pub use storage::StorageBackend;
