//! Prometheus metric name constants.
//!
//! Centralises all metric identifiers so they stay consistent between the
//! recording sites in [`super::queue::QueueManager`] and any dashboards or
//! alerting rules that reference them.

/// Counter incremented each time a job is pushed (enqueued).
pub const JOBS_PUSHED_TOTAL: &str = "rustqueue_jobs_pushed_total";

/// Counter incremented each time a job is acknowledged (completed successfully).
pub const JOBS_COMPLETED_TOTAL: &str = "rustqueue_jobs_completed_total";

/// Counter incremented each time a job is reported as failed.
pub const JOBS_FAILED_TOTAL: &str = "rustqueue_jobs_failed_total";

/// Counter incremented by the number of jobs returned from a pull (dequeue).
pub const JOBS_PULLED_TOTAL: &str = "rustqueue_jobs_pulled_total";
