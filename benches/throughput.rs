//! Throughput benchmarks for RustQueue push / pull / ack operations.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::json;
use tempfile::tempdir;

use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::RedbStorage;

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

criterion_group!(benches, push_single_job, push_pull_ack_roundtrip, batch_push);
criterion_main!(benches);
