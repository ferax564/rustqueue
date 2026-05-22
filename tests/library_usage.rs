//! Integration tests for the RustQueue builder / library API.

use rustqueue::JobState;
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::test]
async fn rustqueue_is_clone_and_shares_state() {
    use rustqueue::RustQueue;
    use serde_json::json;
    let rq = RustQueue::memory().build().unwrap();
    let rq2 = rq.clone();
    let id = rq
        .push("emails", "welcome", json!({"to": "a@b.com"}), None)
        .await
        .unwrap();
    let job = rq2.get_job(id).await.unwrap();
    assert!(job.is_some());
}

#[tokio::test]
async fn test_zero_config_library_usage() {
    let rq = RustQueue::memory().build().unwrap();

    // Push a job
    let id = rq
        .push("emails", "send-welcome", json!({"to": "a@b.com"}), None)
        .await
        .unwrap();

    // Pull it
    let jobs = rq.pull("emails", 1).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, id);
    assert_eq!(jobs[0].state, JobState::Active);

    // Ack it
    rq.ack(id, Some(json!({"sent": true}))).await.unwrap();

    let job = rq.get_job(id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Completed);
}

#[tokio::test]
async fn test_redb_library_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
    let rq = RustQueue::redb(&db_path).unwrap().build().unwrap();

    let id = rq.push("work", "task", json!({}), None).await.unwrap();
    let jobs = rq.pull("work", 1).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, id);
}

#[tokio::test]
async fn build_rejects_zero_tick_interval() {
    use rustqueue::RustQueue;
    use std::time::Duration;
    let err = RustQueue::memory()
        .tick_interval(Duration::ZERO)
        .build()
        .err();
    assert!(err.is_some(), "zero tick_interval must be rejected");
}

#[tokio::test]
async fn embedded_backoff_retry_is_promoted_by_housekeeping() {
    use rustqueue::RustQueue;
    use serde_json::json;
    use std::time::Duration;
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    rq.start_housekeeping().unwrap();
    let id = rq.push("q", "job", json!({}), None).await.unwrap();
    let jobs = rq.pull("q", 1).await.unwrap();
    assert_eq!(jobs.len(), 1);
    rq.fail(id, "boom").await.unwrap();
    let mut promoted = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(j) = rq.get_job(id).await.unwrap() {
            use rustqueue::JobState;
            if matches!(j.state, JobState::Waiting) {
                promoted = true;
                break;
            }
        }
    }
    assert!(promoted, "delayed retry was never promoted back to Waiting");
}

#[test]
fn start_housekeeping_outside_runtime_errors() {
    use rustqueue::RustQueue;
    let rq = RustQueue::memory().build().unwrap();
    assert!(rq.start_housekeeping().is_err());
}

#[tokio::test]
async fn start_housekeeping_is_idempotent() {
    use rustqueue::RustQueue;
    let rq = RustQueue::memory().build().unwrap();
    rq.start_housekeeping().unwrap();
    rq.start_housekeeping().unwrap();
    rq.clone().start_housekeeping().unwrap();
}

#[tokio::test]
async fn dropping_all_clones_aborts_housekeeping() {
    use rustqueue::RustQueue;
    use std::time::Duration;
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    rq.start_housekeeping().unwrap();
    let rq2 = rq.clone();
    drop(rq);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(rq2);
    tokio::time::sleep(Duration::from_millis(100)).await;
}
