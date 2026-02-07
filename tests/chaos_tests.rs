//! Chaos and stability tests for RustQueue storage backends.
//!
//! These tests verify crash recovery, flush correctness, memory stability,
//! and concurrent stress behavior across different storage backends.

use std::sync::Arc;

use serde_json::json;
use tempfile::tempdir;

use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::{
    BufferedRedbConfig, BufferedRedbStorage, HybridConfig, HybridStorage, MemoryStorage,
    RedbStorage, StorageBackend,
};

// ---------------------------------------------------------------------------
// 1. Crash recovery: push 100 jobs -> drop storage -> reopen -> verify
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crash_recovery_redb() {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("crash-test.redb");

    // Phase 1: push 100 jobs and then drop the storage (simulating a crash).
    {
        let storage = Arc::new(RedbStorage::new(&db_path).expect("open redb"));
        let mgr = QueueManager::new(Arc::clone(&storage) as Arc<dyn StorageBackend>);

        for i in 0..100 {
            let name = format!("crash-job-{i}");
            mgr.push("crash-q", &name, json!({"i": i}), None)
                .await
                .expect("push should succeed");
        }

        // Verify all 100 are visible before dropping.
        let counts = storage.get_queue_counts("crash-q").await.unwrap();
        assert_eq!(counts.waiting, 100, "should have 100 waiting before crash");

        // Drop storage — simulates kill -9 / crash.
        drop(mgr);
        drop(storage);
    }

    // Phase 2: reopen from the same path and verify data persisted.
    {
        let storage = Arc::new(RedbStorage::new(&db_path).expect("reopen redb"));
        let counts = storage.get_queue_counts("crash-q").await.unwrap();
        assert_eq!(
            counts.waiting, 100,
            "after crash recovery, all 100 waiting jobs should be present"
        );

        // Verify we can pull them all.
        let pulled = storage.dequeue("crash-q", 100).await.unwrap();
        assert_eq!(pulled.len(), 100, "should be able to dequeue all 100 jobs");
    }
}

// ---------------------------------------------------------------------------
// 2. Hybrid dirty flush: push 50 jobs -> wait for flush -> verify inner redb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hybrid_dirty_flush() {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("hybrid-flush.redb");

    // Phase 1: create hybrid, push jobs, wait for flush, then drop everything.
    {
        let inner = Arc::new(RedbStorage::new(&db_path).expect("create redb"));
        let hybrid = HybridStorage::new(
            inner,
            HybridConfig {
                snapshot_interval_ms: 100,
                max_dirty_before_flush: 10,
            },
        );
        let mgr = QueueManager::new(Arc::new(hybrid));

        // Push 50 jobs — with max_dirty=10, flushes happen automatically.
        for i in 0..50 {
            let name = format!("flush-job-{i}");
            mgr.push("flush-q", &name, json!({"i": i}), None)
                .await
                .expect("push should succeed");
        }

        // Wait long enough for the background flush to persist all dirty entries.
        // With 100ms interval + 10 max_dirty, multiple flushes should have fired.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Drop everything — hybrid, inner, mgr — releasing the redb file lock.
        drop(mgr);
    }

    // Give tokio a moment to clean up the aborted flush task and release the Arc.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Phase 2: open a *new* RedbStorage pointing at the same file to verify persistence.
    let verify_storage = RedbStorage::new(&db_path).expect("reopen redb for verification");
    let counts = verify_storage.get_queue_counts("flush-q").await.unwrap();
    assert_eq!(
        counts.waiting, 50,
        "inner redb should have all 50 jobs after hybrid flush"
    );
}

// ---------------------------------------------------------------------------
// 3. Memory stability: push+pull+ack 10K cycles, check RSS growth
//    Marked #[ignore] because it runs for ~10-20s depending on hardware.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn memory_stability() {
    use sysinfo::{Pid, System};

    let storage = Arc::new(MemoryStorage::new());
    let mgr = QueueManager::new(storage);

    // Warm up
    for i in 0..100 {
        let name = format!("warmup-{i}");
        let id = mgr
            .push("mem-q", &name, json!({"i": i}), None)
            .await
            .unwrap();
        let pulled = mgr.pull("mem-q", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        mgr.ack(id, None).await.unwrap();
    }

    // Measure initial RSS
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new();
    sys.refresh_processes();
    let rss_before = sys
        .process(pid)
        .map(|p| p.memory())
        .unwrap_or(0);

    // Run 10K push+pull+ack cycles
    for i in 0..10_000 {
        let name = format!("stability-{i}");
        let id = mgr
            .push("mem-q", &name, json!({"i": i}), None)
            .await
            .unwrap();
        let pulled = mgr.pull("mem-q", 1).await.unwrap();
        assert_eq!(pulled.len(), 1);
        mgr.ack(id, None).await.unwrap();
    }

    // Measure final RSS
    sys.refresh_processes();
    let rss_after = sys
        .process(pid)
        .map(|p| p.memory())
        .unwrap_or(0);

    let growth_mb = (rss_after as f64 - rss_before as f64) / (1024.0 * 1024.0);
    eprintln!(
        "RSS before: {} MB, after: {} MB, growth: {:.1} MB",
        rss_before / (1024 * 1024),
        rss_after / (1024 * 1024),
        growth_mb
    );

    assert!(
        growth_mb < 50.0,
        "RSS grew by {growth_mb:.1} MB over 10K cycles, expected < 50 MB"
    );
}

// ---------------------------------------------------------------------------
// 4. Concurrent push stress: 100 concurrent pushes via BufferedRedbStorage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_push_stress() {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("stress-test.redb");

    let inner = Arc::new(RedbStorage::new(&db_path).expect("create redb"));
    let buffered = BufferedRedbStorage::new(
        inner,
        BufferedRedbConfig {
            interval_ms: 5,
            max_batch: 50,
        },
    );
    let mgr = Arc::new(QueueManager::new(Arc::new(buffered)));

    // Spawn 100 concurrent push tasks.
    let mut handles = Vec::with_capacity(100);
    for i in 0..100u64 {
        let mgr = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            let name = format!("stress-{i}");
            mgr.push("stress-q", &name, json!({"i": i}), None)
                .await
                .expect("concurrent push should succeed");
        }));
    }

    // Wait for all to complete.
    for h in handles {
        h.await.expect("task should not panic");
    }

    // Give the buffered storage time to flush any pending writes.
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify all 100 jobs are persisted via the QueueManager (reads through
    // the buffered layer to the underlying redb storage).
    let stats = mgr.get_queue_stats("stress-q").await.expect("queue stats");
    assert_eq!(
        stats.waiting, 100,
        "all 100 concurrent pushes should be persisted"
    );

    // Verify we can pull all 100 jobs back out.
    let pulled = mgr.pull("stress-q", 100).await.expect("pull all");
    assert_eq!(
        pulled.len(),
        100,
        "should be able to pull all 100 concurrently pushed jobs"
    );
}
