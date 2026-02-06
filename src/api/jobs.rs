//! Job-related HTTP endpoints.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, AppState};
use crate::engine::models::Job;
use crate::engine::queue::JobOptions;

// ── Request / Response types ────────────────────────────────────────────────

/// Body for pushing a single job.
#[derive(Debug, Deserialize)]
pub struct PushJobRequest {
    pub name: String,
    pub data: serde_json::Value,
    #[serde(flatten)]
    pub options: JobOptions,
}

/// Wrapper to accept either a single job or an array of jobs.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PushJobBody {
    Single(PushJobRequest),
    Batch(Vec<PushJobRequest>),
}

#[derive(Debug, Serialize)]
pub struct PushSingleResponse {
    pub ok: bool,
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct PushBatchResponse {
    pub ok: bool,
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum PushResponse {
    Single(PushSingleResponse),
    Batch(PushBatchResponse),
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    pub count: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct PullSingleResponse {
    pub ok: bool,
    pub job: Option<Job>,
}

#[derive(Debug, Serialize)]
pub struct PullMultiResponse {
    pub ok: bool,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum PullResponse {
    Single(Box<PullSingleResponse>),
    Multi(PullMultiResponse),
}

#[derive(Debug, Serialize)]
pub struct GetJobResponse {
    pub ok: bool,
    pub job: Job,
}

#[derive(Debug, Deserialize)]
pub struct AckRequest {
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct FailRequest {
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct ProgressRequest {
    pub progress: u8,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FailResponse {
    pub ok: bool,
    pub retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/queues/{queue}/jobs", post(push_jobs).get(pull_jobs))
        .route("/api/v1/jobs/{id}", get(get_job))
        .route("/api/v1/jobs/{id}/ack", post(ack_job))
        .route("/api/v1/jobs/{id}/fail", post(fail_job))
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/api/v1/jobs/{id}/progress", post(update_progress))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/v1/queues/:queue/jobs — Push one or more jobs.
async fn push_jobs(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
    Json(body): Json<PushJobBody>,
) -> Result<(StatusCode, Json<PushResponse>), ApiError> {
    match body {
        PushJobBody::Single(req) => {
            let opts = build_options(req.options);
            let id = state
                .queue_manager
                .push(&queue, &req.name, req.data, Some(opts))
                .await?;
            Ok((
                StatusCode::CREATED,
                Json(PushResponse::Single(PushSingleResponse {
                    ok: true,
                    id: id.to_string(),
                })),
            ))
        }
        PushJobBody::Batch(reqs) => {
            let mut ids = Vec::with_capacity(reqs.len());
            for req in reqs {
                let opts = build_options(req.options);
                let id = state
                    .queue_manager
                    .push(&queue, &req.name, req.data, Some(opts))
                    .await?;
                ids.push(id.to_string());
            }
            Ok((
                StatusCode::CREATED,
                Json(PushResponse::Batch(PushBatchResponse { ok: true, ids })),
            ))
        }
    }
}

/// GET /api/v1/queues/:queue/jobs — Pull next job(s).
async fn pull_jobs(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
    Query(query): Query<PullQuery>,
) -> Result<Json<PullResponse>, ApiError> {
    let count = query.count.unwrap_or(1);
    let mut jobs = state.queue_manager.pull(&queue, count).await?;

    if count == 1 {
        let job = if jobs.is_empty() {
            None
        } else {
            Some(jobs.remove(0))
        };
        Ok(Json(PullResponse::Single(Box::new(PullSingleResponse {
            ok: true,
            job,
        }))))
    } else {
        Ok(Json(PullResponse::Multi(PullMultiResponse {
            ok: true,
            jobs,
        })))
    }
}

/// GET /api/v1/jobs/:id — Get job by ID.
async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<GetJobResponse>, ApiError> {
    let job = state
        .queue_manager
        .get_job(id)
        .await?
        .ok_or_else(|| ApiError::from(crate::engine::error::RustQueueError::JobNotFound(id.to_string())))?;
    Ok(Json(GetJobResponse { ok: true, job }))
}

/// POST /api/v1/jobs/:id/ack — Acknowledge completion.
async fn ack_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<AckRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.ack(id, body.result).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/jobs/:id/fail — Report failure.
async fn fail_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<FailRequest>,
) -> Result<Json<FailResponse>, ApiError> {
    let result = state.queue_manager.fail(id, &body.error).await?;
    Ok(Json(FailResponse {
        ok: true,
        retry: result.will_retry,
        next_attempt_at: result.next_attempt_at.map(|t| t.to_rfc3339()),
    }))
}

/// POST /api/v1/jobs/:id/progress — Update job progress.
async fn update_progress(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<ProgressRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state
        .queue_manager
        .update_progress(id, body.progress, body.message)
        .await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/jobs/:id/cancel — Cancel a job.
async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.cancel(id).await?;
    Ok(Json(OkResponse { ok: true }))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn build_options(opts: JobOptions) -> JobOptions {
    opts
}
