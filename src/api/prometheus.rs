//! Prometheus metrics scrape endpoint.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::api::AppState;

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/metrics/prometheus", get(prometheus_metrics))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/metrics/prometheus — render Prometheus text-format metrics.
async fn prometheus_metrics(State(state): State<Arc<AppState>>) -> Response {
    match &state.metrics_handle {
        Some(handle) => {
            let body = handle.render();
            (
                StatusCode::OK,
                [("content-type", "text/plain; charset=utf-8")],
                body,
            )
                .into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Metrics not available",
        )
            .into_response(),
    }
}
