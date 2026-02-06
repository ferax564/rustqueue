//! Integration tests for the RustQueue builder / library API.

use rustqueue::JobState;
use rustqueue::RustQueue;
use serde_json::json;

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
