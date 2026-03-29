//! Persistent queue that survives process restarts.
//!
//! Run this twice to see that jobs pushed in the first run
//! are still available in the second run.
//!
//! Run with: `cargo run --example persistent`

use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = "/tmp/rustqueue-example.db";
    let rq = RustQueue::redb(db_path)?.build()?;

    // Check if there are jobs from a previous run
    let existing = rq.pull("tasks", 1).await?;
    if !existing.is_empty() {
        let job = &existing[0];
        println!(
            "Found job from previous run: {} (data: {})",
            job.name, job.data
        );
        rq.ack(job.id, None).await?;
        println!("Acknowledged. Run again to push a new one.");
        return Ok(());
    }

    // No existing jobs — push one and exit without processing
    let id = rq
        .push(
            "tasks",
            "generate-report",
            json!({"report": "monthly", "month": "march"}),
            None,
        )
        .await?;
    println!("Pushed job {id} to {db_path}");
    println!("Run this example again to see the job survive the restart.");

    Ok(())
}
