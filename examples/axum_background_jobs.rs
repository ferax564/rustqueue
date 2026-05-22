//! Production-shaped background jobs in an Axum web application.
//!
//! Demonstrates a managed worker with graceful shutdown wired to the same
//! Ctrl-C signal as the Axum server — so the worker drains its in-flight job
//! before the process exits.
//!
//! Architecture:
//!   - `POST /enqueue` — pushes an email job to the "emails" queue
//!   - `POST /send-email` — alias kept for backwards compatibility
//!   - `GET  /stats`   — shows queue depth
//!   - A worker is spawned via `tokio::spawn` using `run_worker_with_shutdown`;
//!     it receives the same shutdown signal as Axum's `with_graceful_shutdown`.
//!     On Ctrl-C: Axum stops accepting connections, the worker finishes the
//!     in-flight job (≤200ms), and the process exits cleanly.
//!
//! Run with: `cargo run --example axum_background_jobs --features axum-integration`
//! Then:     `curl -X POST http://localhost:3000/enqueue \
//!              -H 'Content-Type: application/json' \
//!              -d '{"to":"user@example.com"}'`
//! To trigger a retry: use `"to":"fail@example.com"` — the handler returns Err
//! on the first attempt, the engine retries, and it succeeds on the second.

use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::RustQueue;
use rustqueue::axum_integration::RqState;
use serde_json::json;
use std::sync::Arc;

/// POST /enqueue — enqueues an email job (production-shaped route name)
async fn enqueue(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let to = body["to"].as_str().unwrap_or("unknown");
    let id = rq
        .push("emails", "send-welcome", json!({"to": to}), None)
        .await
        .unwrap();
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

/// POST /send-email — backwards-compatible alias for /enqueue
async fn send_email(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    enqueue(rq, Json(body)).await
}

/// GET /stats — shows queue depth
async fn stats(rq: RqState) -> Json<serde_json::Value> {
    let queues = rq.list_queues().await.unwrap();
    Json(json!({"queues": queues}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let rq = RustQueue::redb("/tmp/rq-axum.db")?.build()?;

    // Oneshot channel: Axum's ctrl_c handler signals the worker to drain.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn the managed worker — drains its in-flight job when shutdown fires.
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            let _ = rq
                .run_worker_with_shutdown(
                    "emails",
                    |job| async move {
                        // Simulate ~200ms email send latency.
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                        let to = job.data["to"].as_str().unwrap_or("");
                        // Deterministic failure: addresses containing "fail@" return
                        // Err on the first attempt so the engine exercises retry/DLQ.
                        if to.starts_with("fail@") && job.data["_retry"].is_null() {
                            tracing::warn!(job_id = %job.id, to, "simulated send failure — will retry");
                            return Err::<(), String>(format!("simulated failure for {to}"));
                        }

                        tracing::info!(job_id = %job.id, to, "email sent");
                        Ok::<(), String>(())
                    },
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await;
        })
    };

    // Wrap in Arc for Axum state (required by RqState extractor).
    let app_rq = Arc::new(rq);

    let app = Router::new()
        .route("/enqueue", post(enqueue))
        .route("/send-email", post(send_email))
        .route("/stats", get(stats))
        .with_state(app_rq);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Server running on http://localhost:3000");
    println!("  POST /enqueue  — {{\"to\":\"user@example.com\"}}");
    println!("  POST /enqueue  — {{\"to\":\"fail@example.com\"}} to trigger retry");
    println!("  GET  /stats");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            // Signal the worker to stop after the current job finishes.
            let _ = shutdown_tx.send(());
        })
        .await?;

    // Wait for the worker to finish draining.
    let _ = worker.await;

    Ok(())
}
