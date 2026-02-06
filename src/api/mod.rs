//! HTTP REST API module for RustQueue.
//!
//! Provides axum-based endpoints for job management, queue operations, and health checks.

pub mod health;
pub mod jobs;
pub mod prometheus;
pub mod queues;
pub mod websocket;

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::api::websocket::JobEvent;
use crate::engine::error::RustQueueError;
use crate::engine::queue::QueueManager;

/// Shared application state passed to all handlers via axum's `State` extractor.
pub struct AppState {
    pub queue_manager: Arc<QueueManager>,
    pub start_time: std::time::Instant,
    /// Handle used to render Prometheus metrics.  `None` when the global
    /// recorder has not been installed (e.g. in tests that don't need metrics).
    pub metrics_handle: Option<metrics_exporter_prometheus::PrometheusHandle>,
    /// Broadcast sender for real-time job events (WebSocket streaming).
    pub event_tx: tokio::sync::broadcast::Sender<JobEvent>,
}

/// Build the full API router with all endpoint groups merged.
pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .merge(jobs::routes())
        .merge(queues::routes())
        .merge(health::routes())
        .merge(prometheus::routes())
        .merge(websocket::routes())
        .with_state(state)
}

// ── Error response types ────────────────────────────────────────────────────

/// PRD-compliant error detail object.
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

/// PRD-compliant error response envelope.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub ok: bool,
    pub error: ErrorDetail,
}

/// Wrapper that converts `RustQueueError` into a PRD-compliant JSON response.
pub struct ApiError(pub RustQueueError);

impl From<RustQueueError> for ApiError {
    fn from(err: RustQueueError) -> Self {
        ApiError(err)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError(RustQueueError::ValidationError(rejection.to_string()))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = ErrorResponse {
            ok: false,
            error: ErrorDetail {
                code: self.0.error_code(),
                message: self.0.to_string(),
                details: None,
            },
        };
        (status, Json(body)).into_response()
    }
}
