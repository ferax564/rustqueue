//! Bearer token authentication middleware for the HTTP API.
//!
//! When `AuthConfig.enabled` is `true`, all requests (except public routes like
//! health and metrics) must include an `Authorization: Bearer <token>` header
//! with a token that appears in `AuthConfig.tokens`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::api::{AppState, ErrorDetail, ErrorResponse};

/// Axum middleware that enforces bearer-token authentication.
///
/// If `state.auth_config.enabled` is `false`, all requests pass through
/// unconditionally. When enabled, the middleware extracts the
/// `Authorization: Bearer <token>` header and checks it against the configured
/// token list. Invalid or missing tokens result in a 401 JSON error response.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // If auth is disabled, pass through immediately.
    if !state.auth_config.enabled {
        return next.run(request).await;
    }

    // Extract the Authorization header.
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    match auth_header {
        Some(header_value) if header_value.starts_with("Bearer ") => {
            let token = &header_value[7..]; // Skip "Bearer "
            if state.auth_config.tokens.iter().any(|t| t == token) {
                next.run(request).await
            } else {
                unauthorized_response("Invalid bearer token")
            }
        }
        Some(_) => unauthorized_response("Authorization header must use Bearer scheme"),
        None => unauthorized_response("Missing Authorization header"),
    }
}

/// Build a 401 JSON error response matching the PRD error envelope format.
fn unauthorized_response(message: &str) -> Response {
    let body = ErrorResponse {
        ok: false,
        error: ErrorDetail {
            code: "UNAUTHORIZED",
            message: message.to_string(),
            details: None,
        },
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}
