//! Background jobs in an Axum web application.
//!
//! Demonstrates adding a persistent job queue to an Axum app with
//! zero external dependencies — no Redis, no RabbitMQ, no Docker.
//!
//! Run with: `cargo run --example axum_background_jobs`
//! Then:     `curl -X POST http://localhost:3000/send-email -H 'Content-Type: application/json' -d '{"to":"user@example.com"}'`

use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::RustQueue;
use rustqueue::axum_integration::RqState;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// POST /send-email — enqueues a welcome email job
async fn send_email(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let to = body["to"].as_str().unwrap_or("unknown");
    let id = rq
        .push("emails", "send-welcome", json!({"to": to}), None)
        .await
        .unwrap();
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

/// GET /stats — shows queue depth
async fn stats(rq: RqState) -> Json<serde_json::Value> {
    let queues = rq.list_queues().await.unwrap();
    Json(json!({"queues": queues}))
}

/// Background worker that processes email jobs
async fn email_worker(rq: Arc<RustQueue>) {
    println!("[worker] Listening for email jobs...");
    loop {
        let jobs = rq.pull("emails", 10).await.unwrap();
        for job in &jobs {
            let to = job.data["to"].as_str().unwrap_or("?");
            println!("[worker] Sending email to {to} (job {})", job.id);
            // Simulate sending an email
            tokio::time::sleep(Duration::from_millis(50)).await;
            rq.ack(job.id, None).await.unwrap();
        }
        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("/tmp/rustqueue-axum-example.db")?.build()?;
    let rq = Arc::new(rq);

    // Spawn the background worker
    tokio::spawn(email_worker(Arc::clone(&rq)));

    // Build the Axum app
    let app = Router::new()
        .route("/send-email", post(send_email))
        .route("/stats", get(stats))
        .with_state(rq);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Server running on http://localhost:3000");
    println!(
        "Try: curl -X POST http://localhost:3000/send-email -H 'Content-Type: application/json' -d '{{\"to\":\"user@example.com\"}}'"
    );
    axum::serve(listener, app).await?;

    Ok(())
}
