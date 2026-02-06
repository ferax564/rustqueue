//! Error types for the RustQueue engine.
//!
//! Defines [`RustQueueError`], the unified error enum used across the queue engine,
//! storage layer, and HTTP API.

use thiserror::Error;

/// Unified error type for all RustQueue operations.
///
/// Each variant carries a machine-readable error code (via [`error_code`](RustQueueError::error_code))
/// and a suggested HTTP status code (via [`http_status`](RustQueueError::http_status)).
#[derive(Debug, Error)]
pub enum RustQueueError {
    /// The requested queue does not exist.
    #[error("Queue '{0}' not found")]
    QueueNotFound(String),

    /// The requested job does not exist.
    #[error("Job '{0}' not found")]
    JobNotFound(String),

    /// The requested schedule does not exist.
    #[error("Schedule not found: {0}")]
    ScheduleNotFound(String),

    /// A job is in a state that does not permit the requested operation.
    #[error("Job is in invalid state '{current}' for operation (expected: {expected})")]
    InvalidState {
        /// The actual state the job is in.
        current: String,
        /// The state(s) required for the operation.
        expected: String,
    },

    /// A unique-key constraint was violated.
    #[error("Duplicate unique key '{0}'")]
    DuplicateKey(String),

    /// The target queue is paused and cannot accept new work.
    #[error("Queue '{0}' is paused")]
    QueuePaused(String),

    /// The caller has exceeded the configured rate limit.
    #[error("Rate limit exceeded")]
    RateLimited,

    /// The request lacks valid authentication credentials.
    #[error("Unauthorized")]
    Unauthorized,

    /// A request payload or parameter failed validation.
    #[error("Validation error: {0}")]
    ValidationError(String),

    /// An unexpected internal error occurred.
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl RustQueueError {
    /// Returns a machine-readable error code string suitable for API responses.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::QueueNotFound(_) => "QUEUE_NOT_FOUND",
            Self::JobNotFound(_) => "JOB_NOT_FOUND",
            Self::ScheduleNotFound(_) => "SCHEDULE_NOT_FOUND",
            Self::InvalidState { .. } => "INVALID_STATE",
            Self::DuplicateKey(_) => "DUPLICATE_KEY",
            Self::QueuePaused(_) => "QUEUE_PAUSED",
            Self::RateLimited => "RATE_LIMITED",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::ValidationError(_) => "VALIDATION_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// Returns the HTTP status code that should accompany this error in API responses.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::QueueNotFound(_) => 404,
            Self::JobNotFound(_) => 404,
            Self::ScheduleNotFound(_) => 404,
            Self::InvalidState { .. } => 409,
            Self::DuplicateKey(_) => 409,
            Self::QueuePaused(_) => 503,
            Self::RateLimited => 429,
            Self::Unauthorized => 401,
            Self::ValidationError(_) => 400,
            Self::Internal(_) => 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes_serialize() {
        // QueueNotFound
        let err = RustQueueError::QueueNotFound("emails".into());
        assert_eq!(err.error_code(), "QUEUE_NOT_FOUND");
        assert_eq!(err.http_status(), 404);

        // JobNotFound
        let err = RustQueueError::JobNotFound("abc-123".into());
        assert_eq!(err.error_code(), "JOB_NOT_FOUND");
        assert_eq!(err.http_status(), 404);

        // InvalidState
        let err = RustQueueError::InvalidState {
            current: "completed".into(),
            expected: "active".into(),
        };
        assert_eq!(err.error_code(), "INVALID_STATE");
        assert_eq!(err.http_status(), 409);

        // DuplicateKey
        let err = RustQueueError::DuplicateKey("user-42-welcome".into());
        assert_eq!(err.error_code(), "DUPLICATE_KEY");
        assert_eq!(err.http_status(), 409);

        // QueuePaused
        let err = RustQueueError::QueuePaused("notifications".into());
        assert_eq!(err.error_code(), "QUEUE_PAUSED");
        assert_eq!(err.http_status(), 503);

        // RateLimited
        let err = RustQueueError::RateLimited;
        assert_eq!(err.error_code(), "RATE_LIMITED");
        assert_eq!(err.http_status(), 429);

        // Unauthorized
        let err = RustQueueError::Unauthorized;
        assert_eq!(err.error_code(), "UNAUTHORIZED");
        assert_eq!(err.http_status(), 401);

        // ValidationError
        let err = RustQueueError::ValidationError("payload too large".into());
        assert_eq!(err.error_code(), "VALIDATION_ERROR");
        assert_eq!(err.http_status(), 400);

        // Internal
        let err = RustQueueError::Internal(anyhow::anyhow!("disk full"));
        assert_eq!(err.error_code(), "INTERNAL_ERROR");
        assert_eq!(err.http_status(), 500);
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            RustQueueError::QueueNotFound("emails".into()).to_string(),
            "Queue 'emails' not found"
        );

        assert_eq!(
            RustQueueError::JobNotFound("abc-123".into()).to_string(),
            "Job 'abc-123' not found"
        );

        assert_eq!(
            RustQueueError::InvalidState {
                current: "completed".into(),
                expected: "active".into(),
            }
            .to_string(),
            "Job is in invalid state 'completed' for operation (expected: active)"
        );

        assert_eq!(
            RustQueueError::DuplicateKey("user-42-welcome".into()).to_string(),
            "Duplicate unique key 'user-42-welcome'"
        );

        assert_eq!(
            RustQueueError::QueuePaused("notifications".into()).to_string(),
            "Queue 'notifications' is paused"
        );

        assert_eq!(
            RustQueueError::RateLimited.to_string(),
            "Rate limit exceeded"
        );

        assert_eq!(RustQueueError::Unauthorized.to_string(), "Unauthorized");

        assert_eq!(
            RustQueueError::ValidationError("payload too large".into()).to_string(),
            "Validation error: payload too large"
        );

        assert_eq!(
            RustQueueError::Internal(anyhow::anyhow!("disk full")).to_string(),
            "Internal error: disk full"
        );
    }
}
