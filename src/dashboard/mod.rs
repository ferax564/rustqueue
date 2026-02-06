//! Embedded web dashboard module.
//!
//! Serves the single-page dashboard UI from assets compiled into the binary
//! via [`rust_embed`].

use std::sync::Arc;

use axum::extract::Path as AxumPath;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use rust_embed::Embed;

use crate::api::AppState;

#[derive(Embed)]
#[folder = "dashboard/static"]
struct DashboardAssets;

/// Build the dashboard router.
///
/// - `GET /dashboard` serves `index.html`.
/// - `GET /dashboard/{*path}` serves arbitrary static assets.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", axum::routing::get(index))
        .route("/dashboard/{*path}", axum::routing::get(serve_asset))
}

async fn index() -> impl IntoResponse {
    serve_embedded("index.html")
}

async fn serve_asset(AxumPath(path): AxumPath<String>) -> impl IntoResponse {
    serve_embedded(&path)
}

fn serve_embedded(path: &str) -> axum::response::Response {
    match DashboardAssets::get(path) {
        Some(file) => {
            let ct = content_type(path);
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, ct)],
                file.data.to_vec(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    }
}
