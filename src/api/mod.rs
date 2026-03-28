//! HTTP REST API module for RustQueue.
//!
//! Provides axum-based endpoints for job management, queue operations, and health checks.

pub mod auth;
pub mod health;
pub mod jobs;
pub mod openapi;
pub mod prometheus;
pub mod queues;
pub mod schedules;
pub mod webhooks;
pub mod websocket;

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::api::websocket::JobEvent;
use crate::engine::error::RustQueueError;
use crate::engine::metrics as metric_names;
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
    /// Authentication configuration (bearer tokens).
    pub auth_config: crate::config::AuthConfig,
    /// Rate limiter for auth failures (per-IP lockout).
    pub auth_rate_limiter: auth::AuthRateLimiter,
    /// Webhook manager (None when webhooks are disabled).
    pub webhook_manager: Option<Arc<crate::engine::webhook::WebhookManager>>,
}

/// Build the full API router with all endpoint groups merged.
///
/// Public routes (health, metrics) are always accessible without authentication.
/// Protected routes (jobs, queues, websocket) require a valid bearer token when
/// `auth_config.enabled` is `true`.
pub fn router(state: Arc<AppState>) -> axum::Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Landing page is always public — visitors should see the marketing page
    // even when auth is enabled.
    let mut public = axum::Router::new()
        .merge(health::routes())
        .merge(prometheus::routes())
        .merge(openapi::routes())
        .merge(crate::dashboard::landing_routes());

    let mut protected = axum::Router::new()
        .merge(jobs::routes())
        .merge(queues::routes())
        .merge(schedules::routes())
        .merge(websocket::routes())
        .merge(webhooks::routes());

    // Dashboard SPA is public when auth is disabled, protected when enabled.
    if state.auth_config.enabled {
        protected = protected.merge(crate::dashboard::dashboard_routes());
    } else {
        public = public.merge(crate::dashboard::dashboard_routes());
    }

    let protected = protected.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::auth_middleware,
    ));

    public
        .merge(protected)
        .layer(axum::middleware::from_fn(http_metrics_middleware))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Middleware that records HTTP request metrics (count + latency by method/status class).
async fn http_metrics_middleware(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method().to_string();
    let start = std::time::Instant::now();

    let response = next.run(request).await;

    let status = response.status().as_u16();
    let status_class = match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    };

    metrics::counter!(
        metric_names::HTTP_REQUESTS_TOTAL,
        "method" => method.clone(),
        "status_class" => status_class.to_string(),
    )
    .increment(1);

    metrics::histogram!(
        metric_names::HTTP_REQUEST_DURATION_SECONDS,
        "method" => method,
    )
    .record(start.elapsed().as_secs_f64());

    response
}

// ── Error response types ────────────────────────────────────────────────────

/// PRD-compliant error detail object.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

/// PRD-compliant error response envelope.
#[derive(Debug, Serialize, utoipa::ToSchema)]
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
