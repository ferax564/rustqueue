//! Long-running worker that processes jobs as they arrive.
//!
//! Run with: `cargo run --example worker`
//!
//! In a second terminal, push jobs:
//!   cargo run --example basic

use rustqueue::RustQueue;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("/tmp/rustqueue-worker.db")?.build()?;

    println!("Worker started. Waiting for jobs on 'emails' queue...");
    println!("Push jobs with: cargo run --example basic\n");

    loop {
        let jobs = rq.pull("emails", 5).await?;

        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        for job in &jobs {
            println!("[{}] Processing: {} — {}", job.queue, job.name, job.data);

            // Simulate work
            tokio::time::sleep(Duration::from_millis(100)).await;

            rq.ack(job.id, None).await?;
            println!("[{}] Done: {}", job.queue, job.id);
        }
    }
}
