//! Job-related HTTP endpoints.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::{ApiError, AppState};
use crate::engine::models::Job;
use crate::engine::queue::{BatchPushItem, JobOptions};

// ── Request / Response types ────────────────────────────────────────────────

/// Body for pushing a single job.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PushJobRequest {
    pub name: String,
    pub data: serde_json::Value,
    #[serde(flatten)]
    #[schema(inline)]
    pub options: JobOptions,
}

/// Wrapper to accept either a single job or an array of jobs.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PushJobBody {
    Single(Box<PushJobRequest>),
    Batch(Vec<PushJobRequest>),
}

impl utoipa::PartialSchema for PushJobBody {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{ArrayBuilder, OneOfBuilder, Schema};
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("PushJobRequest"))
                .item(
                    ArrayBuilder::new()
                        .items(Ref::from_schema_name("PushJobRequest"))
                        .build(),
                )
                .build(),
        )
        .into()
    }
}

impl utoipa::ToSchema for PushJobBody {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PushJobBody")
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PushSingleResponse {
    pub ok: bool,
    pub id: String,
}

#[derive(Debug, Serialize, ToSchema)]
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

impl utoipa::PartialSchema for PushResponse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{OneOfBuilder, Schema};
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("PushSingleResponse"))
                .item(Ref::from_schema_name("PushBatchResponse"))
                .build(),
        )
        .into()
    }
}

impl utoipa::ToSchema for PushResponse {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PushResponse")
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PullQuery {
    pub count: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PullSingleResponse {
    pub ok: bool,
    pub job: Option<Job>,
}

#[derive(Debug, Serialize, ToSchema)]
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

impl utoipa::PartialSchema for PullResponse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{OneOfBuilder, Schema};
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("PullSingleResponse"))
                .item(Ref::from_schema_name("PullMultiResponse"))
                .build(),
        )
        .into()
    }
}

impl utoipa::ToSchema for PullResponse {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PullResponse")
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GetJobResponse {
    pub ok: bool,
    pub job: Job,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AckRequest {
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FailRequest {
    pub error: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProgressRequest {
    pub progress: u8,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DlqQueryParams {
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FailResponse {
    pub ok: bool,
    pub retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DlqResponse {
    pub ok: bool,
    pub jobs: Vec<Job>,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/queues/{queue}/jobs",
            post(push_jobs).get(pull_jobs),
        )
        .route("/api/v1/queues/{queue}/dlq", get(get_dlq_jobs))
        .route("/api/v1/jobs/{id}", get(get_job))
        .route("/api/v1/jobs/{id}/ack", post(ack_job))
        .route("/api/v1/jobs/{id}/fail", post(fail_job))
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/api/v1/jobs/{id}/progress", post(update_progress))
        .route("/api/v1/jobs/{id}/heartbeat", post(heartbeat_job))
        .route("/api/v1/flows/{flow_id}", get(get_flow_status))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/v1/queues/:queue/jobs — Push one or more jobs.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{queue}/jobs",
    tag = "Jobs",
    params(("queue" = String, Path, description = "Queue name")),
    request_body = PushJobBody,
    responses(
        (status = 201, description = "Job(s) created", body = PushResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 503, description = "Queue paused"),
    )
)]
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
            let batch_items = reqs
                .into_iter()
                .map(|req| BatchPushItem {
                    name: req.name,
                    data: req.data,
                    options: Some(build_options(req.options)),
                })
                .collect();
            let ids = state.queue_manager.push_batch(&queue, batch_items).await?;
            let ids = ids.into_iter().map(|id| id.to_string()).collect();
            Ok((
                StatusCode::CREATED,
                Json(PushResponse::Batch(PushBatchResponse { ok: true, ids })),
            ))
        }
    }
}

/// GET /api/v1/queues/:queue/jobs — Pull next job(s).
#[utoipa::path(
    get,
    path = "/api/v1/queues/{queue}/jobs",
    tag = "Jobs",
    params(
        ("queue" = String, Path, description = "Queue name"),
        ("count" = Option<u32>, Query, description = "Number of jobs to pull (default: 1)"),
    ),
    responses(
        (status = 200, description = "Job(s) pulled", body = PullResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
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
#[utoipa::path(
    get,
    path = "/api/v1/jobs/{id}",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    responses(
        (status = 200, description = "Job found", body = GetJobResponse),
        (status = 404, description = "Job not found"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<GetJobResponse>, ApiError> {
    let job = state.queue_manager.get_job(id).await?.ok_or_else(|| {
        ApiError::from(crate::engine::error::RustQueueError::JobNotFound(
            id.to_string(),
        ))
    })?;
    Ok(Json(GetJobResponse { ok: true, job }))
}

/// POST /api/v1/jobs/:id/ack — Acknowledge completion.
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/ack",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    request_body(content = Option<AckRequest>, description = "Optional result data"),
    responses(
        (status = 200, description = "Job acknowledged", body = OkResponse),
        (status = 404, description = "Job not found"),
        (status = 409, description = "Invalid job state"),
    )
)]
async fn ack_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Option<Json<AckRequest>>,
) -> Result<Json<OkResponse>, ApiError> {
    let result = body.and_then(|b| b.0.result);
    state.queue_manager.ack(id, result).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/jobs/:id/fail — Report failure.
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/fail",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    request_body = FailRequest,
    responses(
        (status = 200, description = "Failure recorded", body = FailResponse),
        (status = 404, description = "Job not found"),
        (status = 409, description = "Invalid job state"),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/progress",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    request_body = ProgressRequest,
    responses(
        (status = 200, description = "Progress updated", body = OkResponse),
        (status = 404, description = "Job not found"),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/cancel",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    responses(
        (status = 200, description = "Job cancelled", body = OkResponse),
        (status = 404, description = "Job not found"),
        (status = 409, description = "Invalid job state"),
    )
)]
async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.cancel(id).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/jobs/:id/heartbeat — Update heartbeat for an active job.
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/heartbeat",
    tag = "Jobs",
    params(("id" = String, Path, description = "Job UUID")),
    responses(
        (status = 200, description = "Heartbeat recorded", body = OkResponse),
        (status = 404, description = "Job not found"),
    )
)]
async fn heartbeat_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.heartbeat(id).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// GET /api/v1/queues/:queue/dlq — List dead-letter-queue jobs for a queue.
#[utoipa::path(
    get,
    path = "/api/v1/queues/{queue}/dlq",
    tag = "Jobs",
    params(
        ("queue" = String, Path, description = "Queue name"),
        ("limit" = Option<u32>, Query, description = "Maximum DLQ jobs to return (default: 50)"),
    ),
    responses(
        (status = 200, description = "DLQ jobs listed", body = DlqResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn get_dlq_jobs(
    State(state): State<Arc<AppState>>,
    Path(queue): Path<String>,
    Query(params): Query<DlqQueryParams>,
) -> Result<Json<DlqResponse>, ApiError> {
    let limit = params.limit.unwrap_or(50);
    let jobs = state.queue_manager.get_dlq_jobs(&queue, limit).await?;
    Ok(Json(DlqResponse { ok: true, jobs }))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn build_options(opts: JobOptions) -> JobOptions {
    opts
}

// ── Flow status ─────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
struct FlowStatusResponse {
    ok: bool,
    flow_id: String,
    jobs: Vec<Job>,
    summary: FlowSummary,
}

#[derive(Serialize, ToSchema)]
struct FlowSummary {
    total: usize,
    waiting: usize,
    active: usize,
    blocked: usize,
    completed: usize,
    failed: usize,
    dlq: usize,
}

#[utoipa::path(
    get,
    path = "/api/v1/flows/{flow_id}",
    tag = "Flows",
    params(("flow_id" = String, Path, description = "Flow identifier")),
    responses(
        (status = 200, description = "Flow status with all jobs", body = FlowStatusResponse),
    )
)]
async fn get_flow_status(
    State(state): State<Arc<AppState>>,
    Path(flow_id): Path<String>,
) -> Result<Json<FlowStatusResponse>, ApiError> {
    let jobs = state.queue_manager.get_flow_jobs(&flow_id).await?;

    let mut summary = FlowSummary {
        total: jobs.len(),
        waiting: 0,
        active: 0,
        blocked: 0,
        completed: 0,
        failed: 0,
        dlq: 0,
    };

    for job in &jobs {
        match job.state {
            crate::engine::models::JobState::Waiting | crate::engine::models::JobState::Created => {
                summary.waiting += 1
            }
            crate::engine::models::JobState::Active => summary.active += 1,
            crate::engine::models::JobState::Blocked => summary.blocked += 1,
            crate::engine::models::JobState::Completed => summary.completed += 1,
            crate::engine::models::JobState::Failed => summary.failed += 1,
            crate::engine::models::JobState::Dlq => summary.dlq += 1,
            _ => {}
        }
    }

    Ok(Json(FlowStatusResponse {
        ok: true,
        flow_id,
        jobs,
        summary,
    }))
}
