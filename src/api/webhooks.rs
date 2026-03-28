//! HTTP endpoints for webhook management (CRUD).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Serialize;
use uuid::Uuid;

use crate::api::AppState;
use crate::engine::webhook::WebhookInput;

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct WebhookResponse {
    ok: bool,
    webhook: crate::engine::webhook::Webhook,
}

#[derive(Serialize)]
struct WebhooksListResponse {
    ok: bool,
    webhooks: Vec<crate::engine::webhook::Webhook>,
}

#[derive(Serialize)]
struct DeletedResponse {
    ok: bool,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/webhooks", post(create_webhook).get(list_webhooks))
        .route(
            "/api/v1/webhooks/{id}",
            get(get_webhook).delete(delete_webhook),
        )
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    request_body = WebhookInput,
    responses(
        (status = 201, description = "Webhook registered"),
        (status = 400, description = "Invalid input"),
    )
)]
async fn create_webhook(
    State(state): State<Arc<AppState>>,
    Json(input): Json<WebhookInput>,
) -> impl IntoResponse {
    let Some(ref mgr) = state.webhook_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOKS_DISABLED", "message": "Webhooks are not enabled"}})),
        ).into_response();
    };

    if input.url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": {"code": "VALIDATION_ERROR", "message": "url is required"}})),
        ).into_response();
    }

    let webhook = mgr.register(input);
    (
        StatusCode::CREATED,
        Json(WebhookResponse { ok: true, webhook }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    responses(
        (status = 200, description = "List of registered webhooks"),
    )
)]
async fn list_webhooks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(ref mgr) = state.webhook_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOKS_DISABLED", "message": "Webhooks are not enabled"}})),
        ).into_response();
    };

    let webhooks = mgr.list();
    Json(WebhooksListResponse { ok: true, webhooks }).into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/webhooks/{id}",
    tag = "Webhooks",
    params(("id" = Uuid, Path, description = "Webhook ID")),
    responses(
        (status = 200, description = "Webhook details"),
        (status = 404, description = "Webhook not found"),
    )
)]
async fn get_webhook(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(ref mgr) = state.webhook_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOKS_DISABLED", "message": "Webhooks are not enabled"}})),
        ).into_response();
    };

    match mgr.get(id) {
        Some(webhook) => Json(WebhookResponse { ok: true, webhook }).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOK_NOT_FOUND", "message": format!("Webhook '{}' not found", id)}})),
        ).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/webhooks/{id}",
    tag = "Webhooks",
    params(("id" = Uuid, Path, description = "Webhook ID")),
    responses(
        (status = 200, description = "Webhook deleted"),
        (status = 404, description = "Webhook not found"),
    )
)]
async fn delete_webhook(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(ref mgr) = state.webhook_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOKS_DISABLED", "message": "Webhooks are not enabled"}})),
        ).into_response();
    };

    if mgr.delete(id) {
        Json(DeletedResponse { ok: true }).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": {"code": "WEBHOOK_NOT_FOUND", "message": format!("Webhook '{}' not found", id)}})),
        ).into_response()
    }
}
