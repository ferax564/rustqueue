//! Background email notifications with zero infrastructure.
//!
//! A small Axum web app that queues email jobs and processes them
//! in the background. Demonstrates crash-safe retry: kill the process
//! mid-send, restart, and unfinished emails pick up where they left off.
//!
//! Run with: `cargo run --example email_notifications`
//!
//! Then try:
//!   curl -X POST http://localhost:3000/signup \
//!     -H 'Content-Type: application/json' \
//!     -d '{"email":"alice@example.com","name":"Alice"}'
//!
//!   curl -X POST http://localhost:3000/reset-password \
//!     -H 'Content-Type: application/json' \
//!     -d '{"email":"alice@example.com"}'
//!
//!   curl http://localhost:3000/stats

use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::axum_integration::RqState;
use rustqueue::{JobOptions, RustQueue};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// POST /signup — queues a welcome email
async fn signup(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let email = body["email"].as_str().unwrap_or("unknown");
    let name = body["name"].as_str().unwrap_or("User");

    let id = rq
        .push(
            "emails",
            "welcome-email",
            json!({
                "to": email,
                "subject": format!("Welcome, {}!", name),
                "template": "welcome",
                "name": name,
            }),
            None,
        )
        .await
        .unwrap();

    println!("[api] Queued welcome email to {email} (job {id})");
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

/// POST /reset-password — queues a password reset email (high priority)
async fn reset_password(
    rq: RqState,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let email = body["email"].as_str().unwrap_or("unknown");

    let id = rq
        .push(
            "emails",
            "password-reset",
            json!({
                "to": email,
                "subject": "Reset your password",
                "template": "password_reset",
            }),
            Some(JobOptions {
                priority: Some(10),
                max_attempts: Some(5),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    println!("[api] Queued password reset to {email} (job {id}, priority=10)");
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

/// GET /stats — shows queue depth and job counts
async fn stats(rq: RqState) -> Json<serde_json::Value> {
    let counts = rq.get_queue_stats("emails").await.unwrap();
    Json(json!({
        "waiting": counts.waiting,
        "active": counts.active,
        "delayed": counts.delayed,
        "completed": counts.completed,
        "failed": counts.failed,
    }))
}

/// Simulated email sender — fails ~20% of the time to demonstrate retries.
async fn send_email(payload: &serde_json::Value) -> Result<(), String> {
    let to = payload["to"].as_str().unwrap_or("?");
    let subject = payload["subject"].as_str().unwrap_or("(no subject)");

    // Simulate network latency
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Simulate occasional SMTP failures
    if rand::random::<f32>() < 0.2 {
        return Err(format!("SMTP timeout sending to {to}"));
    }

    println!("[worker] Sent: \"{subject}\" → {to}");
    Ok(())
}

/// Background worker that pulls email jobs and sends them.
async fn email_worker(rq: Arc<RustQueue>) {
    println!("[worker] Listening for email jobs...");
    loop {
        let jobs = rq.pull("emails", 5).await.unwrap();
        for job in &jobs {
            let to = job.data["to"].as_str().unwrap_or("?");
            println!(
                "[worker] Processing: {} → {} (attempt {}/{})",
                job.name, to, job.attempt, job.max_attempts
            );

            match send_email(&job.data).await {
                Ok(()) => {
                    rq.ack(job.id, None).await.unwrap();
                }
                Err(e) => {
                    println!("[worker] Failed: {e} — will retry");
                    rq.fail(job.id, &e).await.unwrap();
                }
            }
        }
        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // One line: create a persistent, crash-safe job queue.
    let rq = RustQueue::redb("/tmp/rustqueue-email-example.db")?.build()?;
    let rq = Arc::new(rq);

    // Spawn the background email worker.
    tokio::spawn(email_worker(Arc::clone(&rq)));

    // Build the web app.
    let app = Router::new()
        .route("/signup", post(signup))
        .route("/reset-password", post(reset_password))
        .route("/stats", get(stats))
        .with_state(rq);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("\nEmail notification service running on http://localhost:3000");
    println!("─────────────────────────────────────────────────────────");
    println!("Try:");
    println!("  curl -X POST http://localhost:3000/signup \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -d '{{\"email\":\"alice@example.com\",\"name\":\"Alice\"}}'");
    println!();
    println!("  curl http://localhost:3000/stats");
    println!("─────────────────────────────────────────────────────────");

    axum::serve(listener, app).await?;
    Ok(())
}
