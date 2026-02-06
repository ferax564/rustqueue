//! Integration tests for the background scheduler.
//!
//! Tests that delayed job promotion works end-to-end when the scheduler is running.

use std::sync::Arc;

use serde_json::json;

use rustqueue::engine::queue::{JobOptions, QueueManager};
use rustqueue::engine::scheduler::start_scheduler;
use rustqueue::storage::MemoryStorage;
use rustqueue::JobState;

#[tokio::test]
async fn test_scheduler_promotes_delayed_jobs() {
    let storage = Arc::new(MemoryStorage::new());
    let manager = Arc::new(QueueManager::new(storage));

    // Push a delayed job with a 50ms delay
    let opts = JobOptions {
        delay_ms: Some(50),
        ..Default::default()
    };
    let id = manager
        .push("work", "delayed-task", json!({}), Some(opts))
        .await
        .unwrap();

    // Verify it's in Delayed state and not pullable
    let job = manager.get_job(id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Delayed);

    let pulled = manager.pull("work", 1).await.unwrap();
    assert!(pulled.is_empty(), "Delayed job should not be pullable yet");

    // Start the scheduler with a fast tick (20ms)
    let scheduler = start_scheduler(Arc::clone(&manager), 20, 30_000);

    // Wait for the delay to expire + scheduler tick (50ms delay + 40ms margin)
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Now the job should be promoted to Waiting and pullable
    let pulled = manager.pull("work", 1).await.unwrap();
    assert_eq!(pulled.len(), 1, "Scheduler should have promoted the delayed job");
    assert_eq!(pulled[0].id, id);
    assert_eq!(pulled[0].state, JobState::Active);

    // Clean up
    scheduler.abort();
}

#[tokio::test]
async fn test_scheduler_detects_timed_out_jobs() {
    let storage = Arc::new(MemoryStorage::new());
    let manager = Arc::new(QueueManager::new(storage));

    // Push a job with a very short timeout (10ms)
    let opts = JobOptions {
        timeout_ms: Some(10),
        ..Default::default()
    };
    let id = manager
        .push("work", "timeout-task", json!({}), Some(opts))
        .await
        .unwrap();

    // Pull it to Active
    manager.pull("work", 1).await.unwrap();

    let job = manager.get_job(id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Active);

    // Start the scheduler with a fast tick (20ms)
    let scheduler = start_scheduler(Arc::clone(&manager), 20, 30_000);

    // Wait for timeout + scheduler tick
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Job should have been timed out (moved to Delayed for retry since default max_attempts=3)
    let job = manager.get_job(id).await.unwrap().unwrap();
    assert!(
        matches!(job.state, JobState::Delayed | JobState::Waiting),
        "Timed out job should be retried, got {:?}",
        job.state
    );
    assert_eq!(job.last_error, Some("job timed out".to_string()));

    scheduler.abort();
}
