//! Simplest RustQueue usage: push a job, pull it, acknowledge it.
//!
//! Run with: `cargo run --example basic`

use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // In-memory backend — great for trying things out.
    // Switch to RustQueue::redb("./jobs.db")? for persistence.
    let rq = RustQueue::memory().build()?;

    // Push a job onto the "emails" queue
    let id = rq.push(
        "emails",
        "send-welcome",
        json!({"to": "user@example.com", "template": "welcome"}),
        None,
    ).await?;
    println!("Pushed job: {id}");

    // Pull one job from the queue
    let jobs = rq.pull("emails", 1).await?;
    let job = &jobs[0];
    println!("Processing: {} (data: {})", job.name, job.data);

    // Acknowledge completion
    rq.ack(job.id, None).await?;
    println!("Done!");

    Ok(())
}
