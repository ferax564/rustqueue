//! Queue-related HTTP endpoints.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::{ApiError, AppState};
use crate::engine::models::QueueCounts;
use crate::engine::queue::QueueInfo;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct ListQueuesResponse {
    pub ok: bool,
    pub queues: Vec<QueueInfo>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueueStatsResponse {
    pub ok: bool,
    pub counts: QueueCounts,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OkResponse {
    pub ok: bool,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/queues", get(list_queues))
        .route("/api/v1/queues/{queue}/stats", get(queue_stats))
        .route("/api/v1/queues/{queue}/pause", post(pause_queue))
        .route("/api/v1/queues/{queue}/resume", post(resume_queue))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/queues — List all queues with counts.
#[utoipa::path(
    get,
    path = "/api/v1/queues",
    tag = "Queues",
    responses(
        (status = 200, description = "List of all queues", body = ListQueuesResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn list_queues(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListQueuesResponse>, ApiError> {
    let queues = state.queue_manager.list_queues().await?;
    Ok(Json(ListQueuesResponse { ok: true, queues }))
}

/// GET /api/v1/queues/:queue/stats — Get queue statistics.
#[utoipa::path(
    get,
    path = "/api/v1/queues/{queue}/stats",
    tag = "Queues",
    params(("queue" = String, Path, description = "Queue name")),
    responses(
        (status = 200, description = "Queue statistics", body = QueueStatsResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn queue_stats(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
) -> Result<Json<QueueStatsResponse>, ApiError> {
    let counts = state.queue_manager.get_queue_stats(&queue).await?;
    Ok(Json(QueueStatsResponse { ok: true, counts }))
}

/// POST /api/v1/queues/:queue/pause — Pause a queue (reject new pushes).
#[utoipa::path(
    post,
    path = "/api/v1/queues/{queue}/pause",
    tag = "Queues",
    params(("queue" = String, Path, description = "Queue name")),
    responses(
        (status = 200, description = "Queue paused", body = OkResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn pause_queue(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
) -> Json<OkResponse> {
    state.queue_manager.pause_queue(&queue);
    Json(OkResponse { ok: true })
}

/// POST /api/v1/queues/:queue/resume — Resume a paused queue.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{queue}/resume",
    tag = "Queues",
    params(("queue" = String, Path, description = "Queue name")),
    responses(
        (status = 200, description = "Queue resumed", body = OkResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn resume_queue(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
) -> Json<OkResponse> {
    state.queue_manager.resume_queue(&queue);
    Json(OkResponse { ok: true })
}
