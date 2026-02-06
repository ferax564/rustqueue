//! WebSocket event streaming for real-time job notifications.
//!
//! Clients connect to `GET /api/v1/events` to receive a stream of [`JobEvent`]
//! messages as JSON frames. Events are broadcast via a `tokio::sync::broadcast`
//! channel from the [`QueueManager`] whenever job state changes.

use std::sync::Arc;

use axum::{
    extract::{ws::Message, State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::AppState;

// ── Event type ──────────────────────────────────────────────────────────────

/// A real-time event emitted when a job transitions state.
#[derive(Debug, Clone, Serialize)]
pub struct JobEvent {
    /// Event type: `"job.pushed"`, `"job.completed"`, `"job.failed"`, `"job.cancelled"`.
    pub event: String,
    /// The ID of the job that triggered the event.
    pub job_id: Uuid,
    /// The queue the job belongs to.
    pub queue: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/events", get(ws_handler))
}

// ── Handler ─────────────────────────────────────────────────────────────────

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    let rx = state.event_tx.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx))
}

async fn handle_socket(
    mut socket: axum::extract::ws::WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<JobEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = serde_json::to_string(&event).unwrap_or_default();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break; // Client disconnected
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(dropped = n, "WebSocket client lagged");
                // Continue — client missed some events
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break; // Channel closed
            }
        }
    }
}
