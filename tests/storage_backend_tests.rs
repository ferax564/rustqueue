//! Generic storage backend test harness.
//!
//! The `backend_tests!` macro generates 14 canonical tests for any
//! `StorageBackend` implementation. Each macro invocation creates a module
//! with unique test names so we can run the same suite against every backend.
//!
//! Currently instantiated for:
//! - `MemoryStorage`  (in-memory HashMap)
//! - `RedbStorage`    (embedded ACID store)

use chrono::{Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use rustqueue::engine::models::{Job, JobState, Schedule};
use rustqueue::storage::{MemoryStorage, RedbStorage, StorageBackend};

/// Helper: create a job in the given queue with sensible defaults.
fn test_job(queue: &str) -> Job {
    Job::new(queue, "test-job", json!({"key": "value"}))
}

/// Helper: create a `Schedule` with the given name and queue.
fn test_schedule(name: &str, queue: &str) -> Schedule {
    let now = Utc::now();
    Schedule {
        name: name.to_string(),
        queue: queue.to_string(),
        job_name: "scheduled-job".to_string(),
        job_data: json!({"scheduled": true}),
        cron_expr: Some("0 0 * * *".to_string()),
        every_ms: None,
        timezone: None,
        max_executions: None,
        execution_count: 0,
        paused: false,
        last_run_at: None,
        next_run_at: None,
        created_at: now,
        updated_at: now,
    }
}

/// Generates the full suite of 14 storage backend tests inside a module named `$mod_name`.
///
/// `$factory` must be an expression that evaluates to a ready-to-use `impl StorageBackend`.
macro_rules! backend_tests {
    ($mod_name:ident, $factory:expr) => {
        mod $mod_name {
            use super::*;

            // ── 1. insert_and_get ───────────────────────────────────────

            #[tokio::test]
            async fn insert_and_get() {
                let storage = $factory;
                let job = test_job("emails");

                let id = storage.insert_job(&job).await.unwrap();
                assert_eq!(id, job.id);

                let retrieved = storage
                    .get_job(id)
                    .await
                    .unwrap()
                    .expect("job should exist after insert");

                assert_eq!(retrieved.id, job.id);
                assert_eq!(retrieved.queue, "emails");
                assert_eq!(retrieved.name, "test-job");
                assert_eq!(retrieved.state, JobState::Waiting);
                assert_eq!(retrieved.data, json!({"key": "value"}));
                assert_eq!(retrieved.priority, 0);
                assert_eq!(retrieved.max_attempts, 3);
            }

            // ── 2. get_nonexistent ──────────────────────────────────────

            #[tokio::test]
            async fn get_nonexistent() {
                let storage = $factory;
                let fake_id = Uuid::now_v7();
                let result = storage.get_job(fake_id).await.unwrap();
                assert!(result.is_none(), "random UUID should not match any job");
            }

            // ── 3. update_job ───────────────────────────────────────────

            #[tokio::test]
            async fn update_job() {
                let storage = $factory;
                let mut job = test_job("work");
                storage.insert_job(&job).await.unwrap();

                // Transition to Active.
                job.state = JobState::Active;
                job.updated_at = Utc::now();
                storage.update_job(&job).await.unwrap();

                let retrieved = storage.get_job(job.id).await.unwrap().unwrap();
                assert_eq!(retrieved.state, JobState::Active);
            }

            // ── 4. delete_job ───────────────────────────────────────────

            #[tokio::test]
            async fn delete_job() {
                let storage = $factory;
                let job = test_job("work");
                let id = storage.insert_job(&job).await.unwrap();

                storage.delete_job(id).await.unwrap();

                let result = storage.get_job(id).await.unwrap();
                assert!(result.is_none(), "deleted job should not be retrievable");
            }

            // ── 5. dequeue_fifo ─────────────────────────────────────────

            #[tokio::test]
            async fn dequeue_fifo() {
                let storage = $factory;

                let job1 = test_job("fifo-q");
                let mut job2 = test_job("fifo-q");
                // Ensure deterministic FIFO ordering: job2 has a later created_at.
                job2.created_at = job1.created_at + Duration::seconds(1);

                storage.insert_job(&job1).await.unwrap();
                storage.insert_job(&job2).await.unwrap();

                // Dequeue 1 -- should get job1 (earlier created_at).
                let dequeued = storage.dequeue("fifo-q", 1).await.unwrap();
                assert_eq!(dequeued.len(), 1);
                assert_eq!(dequeued[0].id, job1.id, "FIFO: earlier job should be dequeued first");
                assert_eq!(dequeued[0].state, JobState::Active);
                assert!(dequeued[0].started_at.is_some());
            }

            // ── 6. dequeue_priority ─────────────────────────────────────

            #[tokio::test]
            async fn dequeue_priority() {
                let storage = $factory;

                let mut low = test_job("pri-q");
                low.priority = 1;

                let mut high = test_job("pri-q");
                high.priority = 10;
                // Give `high` a later created_at to prove priority beats FIFO.
                high.created_at = low.created_at + Duration::seconds(5);

                storage.insert_job(&low).await.unwrap();
                storage.insert_job(&high).await.unwrap();

                let dequeued = storage.dequeue("pri-q", 1).await.unwrap();
                assert_eq!(dequeued.len(), 1);
                assert_eq!(
                    dequeued[0].id, high.id,
                    "higher priority job should be dequeued first"
                );
                assert_eq!(dequeued[0].state, JobState::Active);
            }

            // ── 7. dequeue_empty ────────────────────────────────────────

            #[tokio::test]
            async fn dequeue_empty() {
                let storage = $factory;
                let dequeued = storage.dequeue("nonexistent-queue", 5).await.unwrap();
                assert!(dequeued.is_empty(), "dequeue from empty/nonexistent queue should return empty vec");
            }

            // ── 8. queue_counts ─────────────────────────────────────────

            #[tokio::test]
            async fn queue_counts() {
                let storage = $factory;

                let mut j_waiting = test_job("counts-q");
                j_waiting.state = JobState::Waiting;

                let mut j_active = test_job("counts-q");
                j_active.state = JobState::Active;

                let mut j_delayed = test_job("counts-q");
                j_delayed.state = JobState::Delayed;

                let mut j_completed = test_job("counts-q");
                j_completed.state = JobState::Completed;

                let mut j_failed = test_job("counts-q");
                j_failed.state = JobState::Failed;

                let mut j_dlq = test_job("counts-q");
                j_dlq.state = JobState::Dlq;

                // A job in a different queue -- should NOT be counted.
                let j_other = test_job("other-q");

                for job in [
                    &j_waiting, &j_active, &j_delayed, &j_completed, &j_failed, &j_dlq, &j_other,
                ] {
                    storage.insert_job(job).await.unwrap();
                }

                let counts = storage.get_queue_counts("counts-q").await.unwrap();
                assert_eq!(counts.waiting, 1, "waiting count");
                assert_eq!(counts.active, 1, "active count");
                assert_eq!(counts.delayed, 1, "delayed count");
                assert_eq!(counts.completed, 1, "completed count");
                assert_eq!(counts.failed, 1, "failed count");
                assert_eq!(counts.dlq, 1, "dlq count");
            }

            // ── 9. move_to_dlq ──────────────────────────────────────────

            #[tokio::test]
            async fn move_to_dlq() {
                let storage = $factory;

                let mut job = test_job("dlq-q");
                job.state = JobState::Failed;
                storage.insert_job(&job).await.unwrap();

                storage
                    .move_to_dlq(&job, "max retries exceeded")
                    .await
                    .unwrap();

                // Verify state changed to Dlq with reason in last_error.
                let stored = storage.get_job(job.id).await.unwrap().unwrap();
                assert_eq!(stored.state, JobState::Dlq);
                assert_eq!(stored.last_error.as_deref(), Some("max retries exceeded"));

                // Verify get_dlq_jobs returns it.
                let dlq_jobs = storage.get_dlq_jobs("dlq-q", 10).await.unwrap();
                assert_eq!(dlq_jobs.len(), 1);
                assert_eq!(dlq_jobs[0].id, job.id);
            }

            // ── 10. scheduled_jobs ──────────────────────────────────────

            #[tokio::test]
            async fn scheduled_jobs() {
                let storage = $factory;

                // A delayed job whose delay_until is in the past -- should be ready.
                let mut past_job = test_job("sched-q");
                past_job.state = JobState::Delayed;
                past_job.delay_until = Some(Utc::now() - Duration::seconds(60));
                storage.insert_job(&past_job).await.unwrap();

                // A delayed job whose delay_until is in the future -- should NOT be ready.
                let mut future_job = test_job("sched-q");
                future_job.state = JobState::Delayed;
                future_job.delay_until = Some(Utc::now() + Duration::hours(1));
                storage.insert_job(&future_job).await.unwrap();

                let ready = storage.get_ready_scheduled(Utc::now()).await.unwrap();
                assert_eq!(ready.len(), 1);
                assert_eq!(ready[0].id, past_job.id);
            }

            // ── 11. schedules_crud ──────────────────────────────────────

            #[tokio::test]
            async fn schedules_crud() {
                let storage = $factory;

                let schedule = test_schedule("daily-report", "reports");
                storage.upsert_schedule(&schedule).await.unwrap();

                // Should appear in active schedules.
                let active = storage.get_active_schedules().await.unwrap();
                assert_eq!(active.len(), 1);
                assert_eq!(active[0].name, "daily-report");
                assert_eq!(active[0].queue, "reports");

                // Pause it -- should disappear from active schedules.
                let mut paused = schedule.clone();
                paused.paused = true;
                storage.upsert_schedule(&paused).await.unwrap();

                let active = storage.get_active_schedules().await.unwrap();
                assert!(active.is_empty(), "paused schedule should not appear in active list");

                // Delete it entirely.
                storage.delete_schedule("daily-report").await.unwrap();

                // Even un-pausing and re-checking, it should be gone.
                let active = storage.get_active_schedules().await.unwrap();
                assert!(active.is_empty(), "deleted schedule should be gone");
            }

            // ── 12. list_queue_names ────────────────────────────────────

            #[tokio::test]
            async fn list_queue_names() {
                let storage = $factory;

                // Insert jobs into 3 different queues.
                storage.insert_job(&test_job("alpha")).await.unwrap();
                storage.insert_job(&test_job("beta")).await.unwrap();
                storage.insert_job(&test_job("gamma")).await.unwrap();

                let mut names = storage.list_queue_names().await.unwrap();
                names.sort();

                assert_eq!(names, vec!["alpha", "beta", "gamma"]);
            }

            // ── 13. unique_key_lookup ───────────────────────────────────

            #[tokio::test]
            async fn unique_key_lookup() {
                let storage = $factory;

                let mut job = test_job("unique-q");
                job.unique_key = Some("dedup-key-1".to_string());
                storage.insert_job(&job).await.unwrap();

                // Should find the job by unique key.
                let found = storage
                    .get_job_by_unique_key("unique-q", "dedup-key-1")
                    .await
                    .unwrap();
                assert!(found.is_some(), "should find job by unique key");
                assert_eq!(found.unwrap().id, job.id);

                // Mark job as Completed -- lookup should now return None
                // (completed jobs are excluded from unique key lookups).
                let mut completed = job.clone();
                completed.state = JobState::Completed;
                storage.update_job(&completed).await.unwrap();

                let found = storage
                    .get_job_by_unique_key("unique-q", "dedup-key-1")
                    .await
                    .unwrap();
                assert!(
                    found.is_none(),
                    "completed job should be excluded from unique key lookup"
                );
            }

            // ── 14. remove_completed_before ─────────────────────────────

            #[tokio::test]
            async fn remove_completed_before() {
                let storage = $factory;

                // Old completed job (30 days ago).
                let mut old_job = test_job("cleanup-q");
                old_job.state = JobState::Completed;
                old_job.completed_at = Some(Utc::now() - Duration::days(30));
                storage.insert_job(&old_job).await.unwrap();

                // Recent completed job (just now) -- should NOT be removed.
                let mut recent_job = test_job("cleanup-q");
                recent_job.state = JobState::Completed;
                recent_job.completed_at = Some(Utc::now());
                storage.insert_job(&recent_job).await.unwrap();

                // A waiting job -- should never be touched by cleanup.
                let waiting_job = test_job("cleanup-q");
                storage.insert_job(&waiting_job).await.unwrap();

                let cutoff = Utc::now() - Duration::days(7);
                let removed = storage.remove_completed_before(cutoff).await.unwrap();
                assert_eq!(removed, 1, "should remove exactly 1 old completed job");

                // Old job is gone.
                assert!(
                    storage.get_job(old_job.id).await.unwrap().is_none(),
                    "old completed job should be removed"
                );
                // Recent completed job is still there.
                assert!(
                    storage.get_job(recent_job.id).await.unwrap().is_some(),
                    "recent completed job should survive cleanup"
                );
                // Waiting job untouched.
                assert!(
                    storage.get_job(waiting_job.id).await.unwrap().is_some(),
                    "waiting job should not be affected by cleanup"
                );
            }
        }
    };
}

// ── Instantiate for MemoryStorage ───────────────────────────────────────────

backend_tests!(memory_backend, MemoryStorage::new());

// ── Instantiate for RedbStorage ─────────────────────────────────────────────

backend_tests!(redb_backend, {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_owned();
    drop(tmp);
    RedbStorage::new(&path).unwrap()
});
