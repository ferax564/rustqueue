//! Health check endpoint.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::AppState;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub ok: bool,
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_seconds: u64,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/health", get(health_check))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/health — Health check.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    tag = "Health",
    responses(
        (status = 200, description = "Server is healthy", body = HealthResponse),
    )
)]
async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let uptime = state.start_time.elapsed().as_secs();
    Json(HealthResponse {
        ok: true,
        status: "healthy",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: uptime,
    })
}
