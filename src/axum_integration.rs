//! Axum integration for RustQueue.
//!
//! Provides [`RqState`], an Axum extractor that gives handlers direct access
//! to the [`RustQueue`] instance through `Arc<RustQueue>` application state.
//!
//! # Quick start
//!
//! ```ignore
//! use axum::{Router, Json, routing::post};
//! use rustqueue::RustQueue;
//! use rustqueue::axum_integration::RqState;
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! async fn enqueue(rq: RqState) -> Json<serde_json::Value> {
//!     let id = rq.push("emails", "send-welcome", json!({}), None).await.unwrap();
//!     Json(json!({"id": id.to_string()}))
//! }
//!
//! let rq = RustQueue::memory().build().unwrap();
//! let app = Router::new()
//!     .route("/enqueue", post(enqueue))
//!     .with_state(Arc::new(rq));
//! ```

use std::ops::Deref;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::builder::RustQueue;

/// Axum extractor that provides access to the [`RustQueue`] instance.
///
/// Expects `Arc<RustQueue>` as the router state. Use it as a handler
/// parameter and call any `RustQueue` method directly:
///
/// ```ignore
/// async fn handler(rq: RqState) -> impl IntoResponse {
///     rq.push("queue", "job", json!({}), None).await.unwrap();
/// }
/// ```
pub struct RqState(pub Arc<RustQueue>);

impl Deref for RqState {
    type Target = RustQueue;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<Arc<RustQueue>> for RqState {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &Arc<RustQueue>,
    ) -> Result<Self, Self::Rejection> {
        Ok(RqState(Arc::clone(state)))
    }
}
