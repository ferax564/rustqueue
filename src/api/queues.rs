//! Queue-related HTTP endpoints.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde::Serialize;

use crate::api::{ApiError, AppState};
use crate::engine::models::QueueCounts;
use crate::engine::queue::QueueInfo;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ListQueuesResponse {
    pub ok: bool,
    pub queues: Vec<QueueInfo>,
}

#[derive(Debug, Serialize)]
pub struct QueueStatsResponse {
    pub ok: bool,
    pub counts: QueueCounts,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/queues", get(list_queues))
        .route("/api/v1/queues/{queue}/stats", get(queue_stats))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/queues — List all queues with counts.
async fn list_queues(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListQueuesResponse>, ApiError> {
    let queues = state.queue_manager.list_queues().await?;
    Ok(Json(ListQueuesResponse { ok: true, queues }))
}

/// GET /api/v1/queues/:queue/stats — Get queue statistics.
async fn queue_stats(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
) -> Result<Json<QueueStatsResponse>, ApiError> {
    let counts = state.queue_manager.get_queue_stats(&queue).await?;
    Ok(Json(QueueStatsResponse { ok: true, counts }))
}
