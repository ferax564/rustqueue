//! Crash-survival demo: a job survives `kill -9` and completes after restart.
//!
//! Terminal 1:  cargo run --example crash_recovery -- worker
//!   (starts a worker on a redb-backed queue with a short stall timeout)
//! Then:        cargo run --example crash_recovery -- push
//!   (enqueues a 10s job; watch the worker start it)
//! Simulate a crash: in Terminal 1 run  kill -9 <pid>  (pid is printed on start)
//! Restart T1:  cargo run --example crash_recovery -- worker
//!   After the stall timeout the job is reclaimed and runs to completion.

use rustqueue::RustQueue;
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let rq = RustQueue::redb("/tmp/rustqueue-crash-demo.db")?
        .stall_timeout(Duration::from_secs(3))
        .tick_interval(Duration::from_millis(500))
        .build()?;

    match mode.as_str() {
        "push" => {
            let id = rq.push("work", "slow-task", json!({"n": 1}), None).await?;
            println!("pushed job {id}");
        }
        "worker" => {
            println!(
                "worker pid {} — process a slow job, then `kill -9` me mid-job",
                std::process::id()
            );
            rq.run_worker("work", |job| async move {
                println!("[start] {} {}", job.id, job.data);
                for i in 1..=10 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    println!("[work ] {} step {i}/10", job.id);
                }
                println!("[done ] {}", job.id);
                Ok::<(), String>(())
            })
            .await?;
        }
        _ => eprintln!("usage: crash_recovery -- [push|worker]"),
    }
    Ok(())
}
