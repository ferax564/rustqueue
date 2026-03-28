//! Throughput benchmarks for RustQueue push / pull / ack operations.
//!
//! Includes both raw `RedbStorage` baselines and `BufferedRedbStorage` variants
//! to measure write coalescing impact.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;
use tempfile::tempdir;

use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::{
    BufferedRedbConfig, BufferedRedbStorage, HybridConfig, HybridStorage, RedbDurability,
    RedbStorage,
};

/// Create a fresh `QueueManager` backed by a temporary redb database.
///
/// Returns the manager together with the `TempDir` guard so the directory is
/// not deleted while the benchmark is running.
fn temp_manager() -> (QueueManager, tempfile::TempDir) {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("bench.redb");
    let storage = RedbStorage::new(&db_path).expect("failed to create RedbStorage");
    let mgr = QueueManager::new(Arc::new(storage));
    (mgr, dir)
}

/// Create a `QueueManager` backed by `BufferedRedbStorage` for write coalescing.
///
/// Must be called from within a tokio runtime (the buffer spawns a flush task).
fn temp_buffered_manager() -> (QueueManager, tempfile::TempDir) {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("bench-buffered.redb");
    let inner = Arc::new(RedbStorage::new(&db_path).expect("failed to create RedbStorage"));
    let buffered = BufferedRedbStorage::new(
        inner,
        BufferedRedbConfig {
            interval_ms: 10,
            max_batch: 100,
        },
    );
    let mgr = QueueManager::new(Arc::new(buffered));
    (mgr, dir)
}

/// Create a `QueueManager` backed by RedbStorage with Eventual durability.
///
/// Eventual durability queues the fsync and returns earlier, yielding
/// significantly higher sequential throughput than the default Immediate mode.
fn temp_manager_eventual() -> (QueueManager, tempfile::TempDir) {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("bench-eventual.redb");
    let storage = RedbStorage::new_with_durability(&db_path, RedbDurability::Eventual)
        .expect("failed to create RedbStorage");
    let mgr = QueueManager::new(Arc::new(storage));
    (mgr, dir)
}

// ── Baseline: raw RedbStorage ────────────────────────────────────────────────

fn push_single_job(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("push_single_job", |b| {
        let (mgr, _dir) = temp_manager();
        let mut counter: u64 = 0;

        b.to_async(&rt).iter(|| {
            counter += 1;
            let name = format!("job-{counter}");
            let mgr = &mgr;
            async move {
                mgr.push("bench", &name, json!({"n": counter}), None)
                    .await
                    .expect("push failed");
            }
        });
    });
}

fn push_pull_ack_roundtrip(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("push_pull_ack_roundtrip", |b| {
        let (mgr, _dir) = temp_manager();
        let mut counter: u64 = 0;

        b.to_async(&rt).iter(|| {
            counter += 1;
            let name = format!("job-{counter}");
            let mgr = &mgr;
            async move {
                // Push
                let id = mgr
                    .push("bench", &name, json!({"n": counter}), None)
                    .await
                    .expect("push failed");

                // Pull
                let jobs = mgr.pull("bench", 1).await.expect("pull failed");
                assert_eq!(jobs.len(), 1);
                assert_eq!(jobs[0].id, id);

                // Ack
                mgr.ack(id, Some(json!({"ok": true})))
                    .await
                    .expect("ack failed");
            }
        });
    });
}

fn batch_push(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("batch_push");

    for size in [10u64, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let (mgr, _dir) = temp_manager();
            let mut batch_counter: u64 = 0;

            b.to_async(&rt).iter(|| {
                batch_counter += 1;
                let mgr = &mgr;
                let batch = batch_counter;
                async move {
                    for i in 0..size {
                        let name = format!("batch-{batch}-job-{i}");
                        mgr.push("bench", &name, json!({"batch": batch, "i": i}), None)
                            .await
                            .expect("push failed");
                    }
                }
            });
        });
    }

    group.finish();
}

// ── Eventual durability: sequential baselines ────────────────────────────────

fn push_single_job_eventual(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("push_single_job_eventual", |b| {
        let (mgr, _dir) = temp_manager_eventual();
        let mut counter: u64 = 0;

        b.to_async(&rt).iter(|| {
            counter += 1;
            let name = format!("job-{counter}");
            let mgr = &mgr;
            async move {
                mgr.push("bench", &name, json!({"n": counter}), None)
                    .await
                    .expect("push failed");
            }
        });
    });
}

fn push_pull_ack_roundtrip_eventual(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("push_pull_ack_roundtrip_eventual", |b| {
        let (mgr, _dir) = temp_manager_eventual();
        let mut counter: u64 = 0;

        b.to_async(&rt).iter(|| {
            counter += 1;
            let name = format!("job-{counter}");
            let mgr = &mgr;
            async move {
                let id = mgr
                    .push("bench", &name, json!({"n": counter}), None)
                    .await
                    .expect("push failed");
                let jobs = mgr.pull("bench", 1).await.expect("pull failed");
                assert_eq!(jobs.len(), 1);
                assert_eq!(jobs[0].id, id);
                mgr.ack(id, Some(json!({"ok": true})))
                    .await
                    .expect("ack failed");
            }
        });
    });
}

// ── Concurrent push: raw vs buffered ─────────────────────────────────────────
//
// Write coalescing only helps under concurrency — multiple callers pushing
// simultaneously get batched into one fsync. Sequential benchmarks always show
// the buffered variant as slower (flush interval overhead). These concurrent
// benchmarks spawn N tasks that each push a job, measuring wall-clock time for
// all to complete. This is the realistic server scenario (many TCP connections
// pushing simultaneously).

fn concurrent_push_raw(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_push_raw");

    for concurrency in [10u64, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let (mgr, _dir) = temp_manager();
                let mgr = Arc::new(mgr);
                let counter = Arc::new(AtomicU64::new(0));

                b.to_async(&rt).iter(|| {
                    let mgr = Arc::clone(&mgr);
                    let counter = Arc::clone(&counter);
                    async move {
                        let mut handles = Vec::with_capacity(concurrency as usize);
                        for _ in 0..concurrency {
                            let mgr = Arc::clone(&mgr);
                            let n = counter.fetch_add(1, Ordering::Relaxed);
                            handles.push(tokio::spawn(async move {
                                let name = format!("job-{n}");
                                mgr.push("bench", &name, json!({"n": n}), None)
                                    .await
                                    .expect("push failed");
                            }));
                        }
                        for h in handles {
                            h.await.expect("task panicked");
                        }
                    }
                });
            },
        );
    }

    group.finish();
}

fn concurrent_push_buffered(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_push_buffered");

    for concurrency in [10u64, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let (mgr, _dir) = rt.block_on(async { temp_buffered_manager() });
                let mgr = Arc::new(mgr);
                let counter = Arc::new(AtomicU64::new(0));

                b.to_async(&rt).iter(|| {
                    let mgr = Arc::clone(&mgr);
                    let counter = Arc::clone(&counter);
                    async move {
                        let mut handles = Vec::with_capacity(concurrency as usize);
                        for _ in 0..concurrency {
                            let mgr = Arc::clone(&mgr);
                            let n = counter.fetch_add(1, Ordering::Relaxed);
                            handles.push(tokio::spawn(async move {
                                let name = format!("job-{n}");
                                mgr.push("bench", &name, json!({"n": n}), None)
                                    .await
                                    .expect("push failed");
                            }));
                        }
                        for h in handles {
                            h.await.expect("task panicked");
                        }
                    }
                });
            },
        );
    }

    group.finish();
}

/// Create a `QueueManager` backed by `HybridStorage` (memory + redb snapshots).
///
/// Must be called from within a tokio runtime (the hybrid layer spawns a flush task).
fn temp_hybrid_manager() -> (QueueManager, tempfile::TempDir) {
    let dir = tempdir().expect("failed to create tempdir");
    let db_path = dir.path().join("bench-hybrid.redb");
    let inner = Arc::new(RedbStorage::new(&db_path).expect("failed to create RedbStorage"));
    let hybrid = HybridStorage::new(
        inner,
        HybridConfig {
            snapshot_interval_ms: 1000,
            max_dirty_before_flush: 5000,
        },
    );
    let mgr = QueueManager::new(Arc::new(hybrid));
    (mgr, dir)
}

fn concurrent_push_hybrid(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_push_hybrid");

    for concurrency in [10u64, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let (mgr, _dir) = rt.block_on(async { temp_hybrid_manager() });
                let mgr = Arc::new(mgr);
                let counter = Arc::new(AtomicU64::new(0));

                b.to_async(&rt).iter(|| {
                    let mgr = Arc::clone(&mgr);
                    let counter = Arc::clone(&counter);
                    async move {
                        let mut handles = Vec::with_capacity(concurrency as usize);
                        for _ in 0..concurrency {
                            let mgr = Arc::clone(&mgr);
                            let n = counter.fetch_add(1, Ordering::Relaxed);
                            handles.push(tokio::spawn(async move {
                                let name = format!("job-{n}");
                                mgr.push("bench", &name, json!({"n": n}), None)
                                    .await
                                    .expect("push failed");
                            }));
                        }
                        for h in handles {
                            h.await.expect("task panicked");
                        }
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    push_single_job,
    push_single_job_eventual,
    push_pull_ack_roundtrip,
    push_pull_ack_roundtrip_eventual,
    batch_push,
    concurrent_push_raw,
    concurrent_push_buffered,
    concurrent_push_hybrid
);
criterion_main!(benches);
