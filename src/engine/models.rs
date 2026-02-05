use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Time-sortable unique job identifier (UUID v7).
pub type JobId = Uuid;

/// Current state of a job in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Created,
    Waiting,
    Delayed,
    Active,
    Completed,
    Failed,
    Dlq,
    Cancelled,
    Blocked,
}

/// Strategy for increasing delay between retry attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    /// Same delay every retry.
    Fixed,
    /// delay * attempt number.
    Linear,
    /// delay * 2^attempt.
    Exponential,
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self::Exponential
    }
}

/// Ordering strategy for a queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueOrdering {
    Fifo,
    Lifo,
    Priority,
    Fair,
}

impl Default for QueueOrdering {
    fn default() -> Self {
        Self::Fifo
    }
}

/// A single log entry appended by a worker during job processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub message: String,
}

/// A unit of work submitted to a queue for processing by a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    // Identity
    pub id: JobId,
    pub custom_id: Option<String>,
    pub name: String,
    pub queue: String,

    // Payload
    pub data: serde_json::Value,
    pub result: Option<serde_json::Value>,

    // State
    pub state: JobState,
    pub progress: Option<u8>,
    pub logs: Vec<LogEntry>,

    // Scheduling
    pub priority: i32,
    pub delay_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,

    // Retry configuration
    pub max_attempts: u32,
    pub attempt: u32,
    pub backoff: BackoffStrategy,
    pub backoff_delay_ms: u64,
    pub last_error: Option<String>,

    // Constraints
    pub ttl_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub unique_key: Option<String>,

    // Organization
    pub tags: Vec<String>,
    pub group_id: Option<String>,

    // Dependencies
    pub depends_on: Vec<JobId>,
    pub flow_id: Option<String>,

    // Behavior flags
    pub lifo: bool,
    pub remove_on_complete: bool,
    pub remove_on_fail: bool,

    // Worker assignment
    pub worker_id: Option<String>,
    pub last_heartbeat: Option<DateTime<Utc>>,
}

impl Job {
    /// Create a new job with sensible defaults.
    pub fn new(queue: impl Into<String>, name: impl Into<String>, data: serde_json::Value) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            custom_id: None,
            name: name.into(),
            queue: queue.into(),
            data,
            result: None,
            state: JobState::Waiting,
            progress: None,
            logs: Vec::new(),
            priority: 0,
            delay_until: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            max_attempts: 3,
            attempt: 0,
            backoff: BackoffStrategy::default(),
            backoff_delay_ms: 1000,
            last_error: None,
            ttl_ms: None,
            timeout_ms: None,
            unique_key: None,
            tags: Vec::new(),
            group_id: None,
            depends_on: Vec::new(),
            flow_id: None,
            lifo: false,
            remove_on_complete: false,
            remove_on_fail: false,
            worker_id: None,
            last_heartbeat: None,
        }
    }
}

/// Summary counts for a queue, broken down by job state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueCounts {
    pub waiting: u64,
    pub active: u64,
    pub delayed: u64,
    pub completed: u64,
    pub failed: u64,
    pub dlq: u64,
}

/// A recurring or one-time schedule that creates jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub queue: String,
    pub job_name: String,
    pub job_data: serde_json::Value,

    // Timing
    pub cron_expr: Option<String>,
    pub every_ms: Option<u64>,
    pub timezone: Option<String>,

    // Constraints
    pub max_executions: Option<u64>,
    pub execution_count: u64,
    pub paused: bool,

    // Metadata
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_new_defaults() {
        let job = Job::new("emails", "send-welcome", serde_json::json!({"to": "a@b.com"}));

        assert_eq!(job.queue, "emails");
        assert_eq!(job.name, "send-welcome");
        assert_eq!(job.state, JobState::Waiting);
        assert_eq!(job.priority, 0);
        assert_eq!(job.max_attempts, 3);
        assert_eq!(job.attempt, 0);
        assert_eq!(job.backoff, BackoffStrategy::Exponential);
        assert!(!job.lifo);
        assert!(job.depends_on.is_empty());
    }

    #[test]
    fn test_job_serialization_roundtrip() {
        let job = Job::new("test", "test-job", serde_json::json!({"key": "value"}));
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: Job = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, job.id);
        assert_eq!(deserialized.queue, job.queue);
        assert_eq!(deserialized.state, job.state);
    }

    #[test]
    fn test_job_state_serde() {
        let state = JobState::Waiting;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"waiting\"");

        let deserialized: JobState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, JobState::Waiting);
    }

    #[test]
    fn test_backoff_strategy_default() {
        assert_eq!(BackoffStrategy::default(), BackoffStrategy::Exponential);
    }
}
