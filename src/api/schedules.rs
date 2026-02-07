//! Schedule-related HTTP endpoints.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::{ApiError, AppState};
use crate::engine::models::Schedule;
use crate::engine::queue::JobOptions;

// ── Request / Response types ────────────────────────────────────────────────

/// Body for creating a schedule.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateScheduleRequest {
    pub name: String,
    pub queue: String,
    pub job_name: String,
    #[serde(default)]
    pub job_data: serde_json::Value,
    #[serde(default)]
    pub job_options: Option<JobOptions>,
    pub cron_expr: Option<String>,
    pub every_ms: Option<u64>,
    pub timezone: Option<String>,
    pub max_executions: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ScheduleResponse {
    pub ok: bool,
    pub schedule: Schedule,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ScheduleListResponse {
    pub ok: bool,
    pub schedules: Vec<Schedule>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OkResponse {
    pub ok: bool,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/schedules",
            post(create_schedule).get(list_schedules),
        )
        .route(
            "/api/v1/schedules/{name}",
            get(get_schedule).delete(delete_schedule),
        )
        .route("/api/v1/schedules/{name}/pause", post(pause_schedule))
        .route("/api/v1/schedules/{name}/resume", post(resume_schedule))
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/v1/schedules — Create a new schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules",
    tag = "Schedules",
    request_body = CreateScheduleRequest,
    responses(
        (status = 201, description = "Schedule created", body = ScheduleResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn create_schedule(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<ScheduleResponse>), ApiError> {
    let now = Utc::now();
    let schedule = Schedule {
        name: req.name,
        queue: req.queue,
        job_name: req.job_name,
        job_data: req.job_data,
        job_options: req.job_options,
        cron_expr: req.cron_expr,
        every_ms: req.every_ms,
        timezone: req.timezone,
        max_executions: req.max_executions,
        execution_count: 0,
        paused: false,
        last_run_at: None,
        next_run_at: None,
        created_at: now,
        updated_at: now,
    };
    state.queue_manager.create_schedule(&schedule).await?;
    Ok((
        StatusCode::CREATED,
        Json(ScheduleResponse { ok: true, schedule }),
    ))
}

/// GET /api/v1/schedules — List all schedules.
#[utoipa::path(
    get,
    path = "/api/v1/schedules",
    tag = "Schedules",
    responses(
        (status = 200, description = "List of all schedules", body = ScheduleListResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn list_schedules(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ScheduleListResponse>, ApiError> {
    let schedules = state.queue_manager.list_schedules().await?;
    Ok(Json(ScheduleListResponse {
        ok: true,
        schedules,
    }))
}

/// GET /api/v1/schedules/:name — Get a single schedule by name.
#[utoipa::path(
    get,
    path = "/api/v1/schedules/{name}",
    tag = "Schedules",
    params(("name" = String, Path, description = "Schedule name")),
    responses(
        (status = 200, description = "Schedule found", body = ScheduleResponse),
        (status = 404, description = "Schedule not found"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<ScheduleResponse>, ApiError> {
    let schedule = state
        .queue_manager
        .get_schedule(&name)
        .await?
        .ok_or_else(|| {
            ApiError::from(crate::engine::error::RustQueueError::ScheduleNotFound(
                name.clone(),
            ))
        })?;
    Ok(Json(ScheduleResponse { ok: true, schedule }))
}

/// DELETE /api/v1/schedules/:name — Delete a schedule.
#[utoipa::path(
    delete,
    path = "/api/v1/schedules/{name}",
    tag = "Schedules",
    params(("name" = String, Path, description = "Schedule name")),
    responses(
        (status = 200, description = "Schedule deleted", body = OkResponse),
        (status = 404, description = "Schedule not found"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.delete_schedule(&name).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/schedules/:name/pause — Pause a schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules/{name}/pause",
    tag = "Schedules",
    params(("name" = String, Path, description = "Schedule name")),
    responses(
        (status = 200, description = "Schedule paused", body = OkResponse),
        (status = 404, description = "Schedule not found"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn pause_schedule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.pause_schedule(&name).await?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/v1/schedules/:name/resume — Resume a paused schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules/{name}/resume",
    tag = "Schedules",
    params(("name" = String, Path, description = "Schedule name")),
    responses(
        (status = 200, description = "Schedule resumed", body = OkResponse),
        (status = 404, description = "Schedule not found"),
        (status = 401, description = "Unauthorized"),
    )
)]
async fn resume_schedule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.queue_manager.resume_schedule(&name).await?;
    Ok(Json(OkResponse { ok: true }))
}
