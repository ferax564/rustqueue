//! OpenAPI specification assembly and serving.
//!
//! Aggregates all annotated handlers and schema types into a single OpenAPI 3.1
//! document, served as JSON at `/api/v1/openapi.json` and rendered via Scalar UI
//! at `/api/v1/docs`.

use std::sync::Arc;

use axum::Router;
use utoipa::OpenApi;

use crate::api::AppState;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "RustQueue API",
        version = "0.12.0",
        description = "Background jobs without infrastructure — embeddable job queue with zero external dependencies.",
        license(name = "MIT OR Apache-2.0"),
    ),
    paths(
        // Jobs (9)
        crate::api::jobs::push_jobs,
        crate::api::jobs::pull_jobs,
        crate::api::jobs::get_job,
        crate::api::jobs::ack_job,
        crate::api::jobs::fail_job,
        crate::api::jobs::update_progress,
        crate::api::jobs::cancel_job,
        crate::api::jobs::heartbeat_job,
        crate::api::jobs::get_dlq_jobs,
        // Queues (4)
        crate::api::queues::list_queues,
        crate::api::queues::queue_stats,
        crate::api::queues::pause_queue,
        crate::api::queues::resume_queue,
        // Schedules (6)
        crate::api::schedules::create_schedule,
        crate::api::schedules::list_schedules,
        crate::api::schedules::get_schedule,
        crate::api::schedules::delete_schedule,
        crate::api::schedules::pause_schedule,
        crate::api::schedules::resume_schedule,
        // Health (1)
        crate::api::health::health_check,
        // Metrics (1)
        crate::api::prometheus::prometheus_metrics,
        // WebSocket (1)
        crate::api::websocket::ws_handler,
    ),
    components(schemas(
        // Core models
        crate::engine::models::Job,
        crate::engine::models::JobState,
        crate::engine::models::BackoffStrategy,
        crate::engine::models::QueueOrdering,
        crate::engine::models::LogEntry,
        crate::engine::models::QueueCounts,
        crate::engine::models::Schedule,
        crate::engine::queue::JobOptions,
        crate::engine::queue::QueueInfo,
        crate::engine::queue::BatchAckResult,
        // Jobs API
        crate::api::jobs::PushJobRequest,
        crate::api::jobs::PushJobBody,
        crate::api::jobs::PushSingleResponse,
        crate::api::jobs::PushBatchResponse,
        crate::api::jobs::PushResponse,
        crate::api::jobs::PullQuery,
        crate::api::jobs::PullSingleResponse,
        crate::api::jobs::PullMultiResponse,
        crate::api::jobs::PullResponse,
        crate::api::jobs::GetJobResponse,
        crate::api::jobs::AckRequest,
        crate::api::jobs::OkResponse,
        crate::api::jobs::FailRequest,
        crate::api::jobs::FailResponse,
        crate::api::jobs::ProgressRequest,
        crate::api::jobs::DlqQueryParams,
        crate::api::jobs::DlqResponse,
        // Queues API
        crate::api::queues::ListQueuesResponse,
        crate::api::queues::QueueStatsResponse,
        // Schedules API
        crate::api::schedules::CreateScheduleRequest,
        crate::api::schedules::ScheduleResponse,
        crate::api::schedules::ScheduleListResponse,
        // Health API
        crate::api::health::HealthResponse,
        // WebSocket
        crate::api::websocket::JobEvent,
        // Error
        crate::api::ErrorDetail,
        crate::api::ErrorResponse,
    )),
    tags(
        (name = "Jobs", description = "Job lifecycle: push, pull, ack, fail, cancel, progress, heartbeat"),
        (name = "Queues", description = "Queue listing, statistics, pause/resume"),
        (name = "Schedules", description = "Recurring schedule management (cron + interval)"),
        (name = "Health", description = "Server health and readiness"),
        (name = "Metrics", description = "Prometheus metrics scrape endpoint"),
        (name = "WebSocket", description = "Real-time job event streaming"),
    )
)]
pub struct ApiDoc;

/// Build a router that serves the OpenAPI JSON spec and Scalar UI.
pub fn routes() -> Router<Arc<AppState>> {
    use utoipa_scalar::{Scalar, Servable};

    let spec = ApiDoc::openapi();

    // Scalar produces a Router<()>. We need to serve it as part of a
    // Router<Arc<AppState>>. We build the scalar router separately and
    // convert it via `with_state(())` which erases the state requirement.
    let scalar_router: Router<()> = Scalar::with_url("/api/v1/docs", spec.clone()).into();

    Router::<Arc<AppState>>::new()
        .route(
            "/api/v1/openapi.json",
            axum::routing::get({
                let spec = spec.clone();
                move || {
                    let spec = spec.clone();
                    async move { axum::Json(spec) }
                }
            }),
        )
        .merge(scalar_router.with_state(()))
}
