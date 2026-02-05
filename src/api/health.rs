//! Health check endpoint.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde::Serialize;

use crate::api::AppState;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
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
async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let uptime = state.start_time.elapsed().as_secs();
    Json(HealthResponse {
        ok: true,
        status: "healthy",
        version: "0.1.0",
        uptime_seconds: uptime,
    })
}
