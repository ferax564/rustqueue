# RustQueue Performance Analysis

**Date:** February 6, 2026
**Version:** v0.9 (A1 + A3 + B1/B2 + atomic ack + durability modes + batch ack coalescing + TCP batch commands)
**Status:** MAJOR IMPROVEMENT IN BATCHED MODE — single-job path still far below target

---

## Executive Summary

RustQueue's single-job throughput with the default redb backend remains far below the PRD target of 50,000 push/sec. However, adding write-coalescing primitives (`complete_jobs_batch`) and TCP batch commands (`push_batch`, `ack_batch`) produced a major throughput jump in batched mode. The remaining bottleneck is mostly on single-job command paths and command-level round-trips.

---

## Implementation Update (v0.9)

Implemented in this phase:

1. **Write coalescing primitive added**: storage trait now supports `complete_jobs_batch()` for batched completion/ack.
2. **redb batch completion override added**: redb completes many jobs in one write transaction/commit.
3. **TCP batch commands added**: protocol now supports `push_batch` and `ack_batch`.
4. **Benchmark harness updated**: competitor runner now supports `--rustqueue-tcp-batch-size` to exercise single vs batched TCP mode.
5. **Durability modes retained**: redb still supports `none`/`eventual`/`immediate`.

Current high-throughput competitor suite run (`scripts/benchmark_competitors.py --ops 500 --repeats 2 --redb-durability immediate --rustqueue-tcp-batch-size 50`):

| System | Produce ops/s | Consume ops/s | End-to-end ops/s |
|--------|---------------:|--------------:|-----------------:|
| rustqueue_http | 245 | 105 | 73 |
| rustqueue_tcp | 10,929 | 5,970 | 3,692 |
| redis_list | 3,005 | 3,448 | 1,704 |
| rabbitmq | 27,637 | 1,922 | 1,519 |
| bullmq | 1,728 | 1,804 | 545 |
| celery | 983 | 513 | 342 |

Interpretation:

- Batched TCP now coalesces write commits for both push and ack paths, and reduces protocol round-trips substantially.
- In this profile, RustQueue TCP is now the fastest system on consume and end-to-end.
- Produce throughput is still below RabbitMQ in this run.
- This is not a pure single-job comparison: RustQueue TCP is explicitly running with `batch_size=50`.
- Single-job control remains much lower (`docs/competitor-benchmark-2026-02-06-immediate-r2-latest.json`: TCP ~334 produce / ~73 consume / ~56 end-to-end).

---

## Benchmark Results (v0.4)

### Criterion Benchmarks (redb backend, release mode)

| Operation | Time | Implied Throughput |
|-----------|------|--------------------|
| push_single_job | 2.93 ms | ~341 ops/sec |
| push_pull_ack_roundtrip | 8.54 ms | ~117 cycles/sec |
| batch_push/10 | 35.0 ms | ~286 jobs/sec |
| batch_push/100 | 465 ms | ~215 jobs/sec |
| batch_push/1000 | 6.44 s | ~155 jobs/sec |

### Live Server Benchmarks (HTTP via Python urllib)

| Operation | Throughput |
|-----------|-----------|
| Push (sequential) | ~224 ops/sec |
| Pull (sequential) | ~93 ops/sec |
| Full lifecycle (push+pull+ack) | ~44 cycles/sec |

### Live Server Benchmarks (TCP via Python socket)

| Operation | Throughput |
|-----------|-----------|
| Push (sequential) | ~83 ops/sec |
| Pull (sequential) | ~28 ops/sec |
| Full lifecycle | ~17 cycles/sec |

### Resource Usage

| Metric | Measured | Target | Status |
|--------|----------|--------|--------|
| Binary size (release) | 6.8 MB | < 15 MB | PASS |
| Memory (idle) | ~15 MB | < 20 MB | PASS |
| Memory (loaded) | ~32 MB | < 100 MB | PASS |
| Startup time | ~10 ms | < 500 ms | PASS |
| Push throughput | ~340/sec | 50,000/sec | FAIL (147x gap) |
| Push+ack throughput | ~117/sec | 30,000/sec | FAIL (256x gap) |
| P50 latency (push) | ~2.9 ms | < 1 ms | FAIL (2.9x gap) |

---

## Root Cause Analysis

### 1. Per-Operation fsync (redb write model) — PRIMARY BOTTLENECK

**Location:** `src/storage/redb.rs:247-266` (insert) and `src/storage/redb.rs:317-344` (update)

Every call to `insert_job()` creates its own write transaction:

```rust
async fn insert_job(&self, job: &Job) -> Result<JobId> {
    let write_txn = self.db.begin_write()?;  // acquire exclusive write lock
    {
        let mut table = write_txn.open_table(JOBS_TABLE)?;
        table.insert(key, value.as_slice())?;
    }
    write_txn.commit()?;  // fsync to disk — ~2.9ms
    Ok(id)
}
```

redb's `commit()` calls `fsync()` to guarantee ACID durability. On typical SSDs, fsync takes 1-5ms. This creates a hard ceiling: **~340-1000 write operations per second**, regardless of CPU or memory.

The same pattern repeats in `update_job()`, `delete_job()`, `move_to_dlq()`, and all other write methods. Each is an independent transaction with its own fsync.

**Impact:** Batch pushes of 1000 jobs take 6.44 seconds because each job gets its own write transaction (1000 × fsync).

### 2. Full-table dequeue scan removed for hot paths — RESOLVED IN v0.6

**Location:** `src/storage/redb.rs`

`dequeue()` now performs a prefix-range scan over the queue/state/priority index and only loads selected job IDs from the main table. The same index-driven pattern now powers `get_queue_counts()`, `get_dlq_jobs()`, and `get_active_jobs()`, while `get_ready_scheduled()` scans delayed-state index entries instead of the full jobs table.

**Impact:** The asymptotic read/query behavior is improved, especially for large databases with many queues/states.

### 3. No Batch Transaction Support — RESOLVED IN v0.5

**Current locations:** `src/storage/mod.rs`, `src/engine/queue.rs`, `src/api/jobs.rs`

This was true in v0.4 and is now fixed.

The storage trait now exposes `insert_jobs_batch(&self, jobs: &[Job])`, redb implements it as a single transaction commit, and the HTTP batch push handler uses `QueueManager::push_batch()` instead of calling single push in a loop.

```rust
let ids = queue_manager.push_batch(queue, items).await?;
// -> storage.insert_jobs_batch() -> begin_write + N inserts + single commit/fsync
```

This removes the guaranteed N x fsync penalty for API batch pushes.

### 4. Remaining scan-heavy calls

Some operations still iterate large portions of `JOBS_TABLE`, notably:

| Method | Current behavior |
|--------|------------------|
| `remove_completed_before()` | scans all jobs and filters by completed timestamp |
| `remove_failed_before()` | scans all jobs and filters by failed + updated_at |
| `remove_dlq_before()` | scans all jobs and filters by DLQ + updated_at |
| `list_queue_names()` | scans all jobs and deduplicates queue names |
| `get_job_by_unique_key()` | scans all jobs and filters by queue + unique key |

These calls are less hot than dequeue/ack in the current benchmark but still matter for long-running nodes and scheduler ticks.

### 5. Blocking Async Runtime — RESOLVED IN v0.5

**Current location:** `src/storage/redb.rs`

```rust
self.run_blocking("insert_job", move |db| {
    // synchronous redb I/O
}).await
```

redb operations are still synchronous I/O, but they now run on Tokio's blocking pool via `spawn_blocking`, preventing runtime worker starvation under load.

### 6. Ack round-trip overhead reduced — RESOLVED IN v0.7

`ack()` now delegates to storage-level `complete_job()` so redb can do state validation and completion update/delete in one transaction:

- trait API: `src/storage/mod.rs`
- queue manager usage: `src/engine/queue.rs`
- redb override: `src/storage/redb.rs`

This reduced end-to-end overhead, but does not remove multi-transition commit costs for the full lifecycle.

### 7. Write amplification from durable index maintenance

Single-job transitions now write `JOBS_TABLE` plus two secondary indexes in the same transaction. For state changes (`Waiting -> Active -> Completed`, retries, DLQ transitions), index rows are removed and reinserted. This improves read complexity but increases per-operation write work on the already fsync-bound path.

---

## Comparison to Targets

| Metric | Current | Target | Gap | Priority |
|--------|---------|--------|-----|----------|
| Push throughput (TCP, single-job profile) | ~334/sec | 50,000/sec | 150x | P0 |
| Push+ack throughput (TCP, single-job profile) | ~56/sec | 30,000/sec | 538x | P0 |
| Push throughput (TCP, batch_size=50 profile) | ~10,929/sec | 50,000/sec | 4.6x | P0 |
| Push+ack throughput (TCP, batch_size=50 profile) | ~3,692/sec | 30,000/sec | 8.1x | P0 |
| P50 push latency (criterion, last measured) | ~2.9 ms | < 1 ms | 2.9x | P1 |
| Dequeue at high cardinality | Index prefix scan | O(log n) | Improved | P1 |
| Batch push 1000 (criterion, v0.5 run) | ~3.30s | < 100ms | 33x | P0 |
| Binary size | 6.8 MB | < 15 MB | OK | - |
| Memory (idle) | ~15 MB | < 20 MB | OK | - |
| Startup | ~10 ms | < 500 ms | OK | - |

---

## Optimization Plan

### Phase A: Quick Wins (estimated 5-10x improvement)

#### A1. Batch Transaction API — DONE (v0.5)

Implemented `insert_jobs_batch(&self, jobs: &[Job])` in `StorageBackend` and redb. API batch push now routes through `QueueManager::push_batch()` to use this path.

```rust
async fn insert_jobs_batch(&self, jobs: &[Job]) -> Result<Vec<JobId>> {
    let write_txn = self.db.begin_write()?;
    let ids = {
        let mut table = write_txn.open_table(JOBS_TABLE)?;
        jobs.iter().map(|job| {
            let key = job.id.as_bytes().as_slice();
            let value = serde_json::to_vec(job)?;
            table.insert(key, value.as_slice())?;
            Ok(job.id)
        }).collect::<Result<Vec<_>>>()?
    };
    write_txn.commit()?; // ONE fsync for all jobs
    Ok(ids)
}
```

**Measured impact so far:** the current throughput suite improved materially (see v0.5 update table above), but still not at target; additional index and write-coalescing work is required.

#### A2. Write Coalescing / Buffered Writes

Buffer individual writes in memory and flush to redb at configurable intervals (e.g., every 10ms or every 100 writes, whichever comes first).

```rust
struct BufferedRedbStorage {
    db: Database,
    buffer: Mutex<Vec<WriteOp>>,
    flush_interval: Duration,
}
```

**Trade-off:** Slightly reduced durability (up to flush_interval of data loss on crash) for dramatically higher throughput. Make this configurable so users choose their durability/throughput balance.

**Expected impact:** Individual pushes go from ~340/sec → ~5,000-10,000/sec (amortized fsync across multiple operations).

#### A3. spawn_blocking for redb Operations — DONE (v0.5)

Wrap all redb calls in `tokio::task::spawn_blocking()` to prevent blocking the async runtime:

```rust
async fn insert_job(&self, job: &Job) -> Result<JobId> {
    let job = job.clone();
    self.run_blocking("insert_job", move |db| {
        // ... synchronous redb operations
    }).await
}
```

**Measured impact so far:** single-operation latency stayed roughly flat while batch and concurrency behavior improved; this change primarily protects runtime responsiveness under concurrent load.

### Phase B: Index Optimization (estimated 10-50x improvement for queries)

#### B1. Secondary Index Tables in redb — DONE (v0.6)

Add auxiliary redb tables for common query patterns:

```rust
// Queue+State index: key = (queue, state, priority, created_at, job_id)
const QUEUE_STATE_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("jobs_queue_state_priority_idx");

// State index: key = (state, updated_at, job_id)
const STATE_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("jobs_state_updated_idx");
```

- `dequeue("emails", 10)` now uses range scan on queue/state prefix (priority-sorted keyspace)
- `get_active_jobs()` now uses state prefix range scan
- `get_ready_scheduled()` now scans delayed-state index entries then filters by `delay_until`

**Observed impact:** Query complexity is improved, but single-job benchmark throughput did not improve yet because write-path durability and index maintenance dominate.

#### B2. Maintain Indexes in Same Transaction — DONE (v0.6)

All write operations now update both index tables in the same transaction as `JOBS_TABLE` writes. This keeps consistency, but it increases per-operation write work (remove old keys + insert new keys on state transitions).

### Phase C: Architecture Changes (estimated 50-100x improvement)

#### C1. In-Memory Hot Path with Periodic Snapshots

For maximum throughput, use MemoryStorage as the hot path with periodic snapshot-to-disk:

```rust
struct HybridStorage {
    hot: MemoryStorage,         // All reads/writes go here
    cold: RedbStorage,          // Periodic snapshots
    snapshot_interval: Duration, // e.g., every 1 second
}
```

**Trade-off:** Up to `snapshot_interval` of data loss on crash. Best for high-throughput scenarios where occasional job re-delivery is acceptable.

**Expected impact:** Push throughput bounded by memory speed: 100,000-500,000 ops/sec.

#### C2. Per-Queue Sharding

Partition jobs into per-queue redb tables instead of a single flat table:

```rust
// Instead of one JOBS_TABLE for all queues:
// "jobs:emails", "jobs:webhooks", "jobs:reports"
fn queue_table(queue: &str) -> TableDefinition<&[u8], &[u8]> {
    TableDefinition::new(&format!("jobs:{}", queue))
}
```

**Expected impact:** Dequeue only scans jobs in the target queue. With 10 queues of 10K jobs each, dequeue scans 10K instead of 100K.

#### C3. Lock-Free MemoryStorage

Replace `RwLock<HashMap>` with lock-free concurrent data structures (e.g., `dashmap`, `crossbeam-skiplist`) for the in-memory backend.

**Expected impact:** Better scaling under high thread contention.

### Phase D: Protocol Optimizations

#### D1. Connection Pooling and Pipelining (TCP)

Allow TCP clients to pipeline multiple commands without waiting for responses. Batch multiple responses into a single write.

#### D2. Binary Protocol Option

JSON serialization/deserialization adds overhead (~50μs per job). A binary protocol (MessagePack or custom) would reduce this by 5-10x.

---

## Recommended Implementation Order

| Priority | Optimization | Status | Effort | Impact | Risk |
|----------|-------------|--------|--------|--------|------|
| 1 | A1: Batch transaction API | Done (v0.5) | Small | 10-20x for batches | Low |
| 2 | A3: spawn_blocking | Done (v0.5) | Small | Better concurrency | Low |
| 3 | B1+B2: Secondary index tables | Done (v0.6) | Medium | Better query scaling | Medium |
| 4 | Atomic ack completion path | Done (v0.7) | Small | Fewer hot-path round trips | Low |
| 5 | redb durability mode (`none`/`eventual`/`immediate`) | Done (v0.8) | Small | Workload dependent | Medium |
| 6 | A2: Write coalescing (`complete_jobs_batch`) | Done (v0.9, partial) | Medium | Large for ack-heavy batches | Medium |
| 7 | D1: TCP batch commands (`push_batch`/`ack_batch`) | Done (v0.9, first pass) | Medium | Large for TCP throughput | Low |
| 8 | A2b: Automatic timed write coalescing (no client batching required) | Next | Medium | 5-10x for singles | Medium (durability trade-off) |
| 9 | C1: Hybrid memory+disk | Planned | Large | 50-100x | High (durability trade-off) |
| 10 | C2: Per-queue sharding | Planned | Medium | 2-10x for dequeue | Low |
| 11 | D1b: TCP pipelining for mixed command streams | Planned | Medium | 2-5x for TCP | Low |
| 12 | C3: Lock-free memory | Planned | Small | 2-3x under contention | Low |
| 13 | D2: Binary protocol | Planned | Large | 2-5x | Medium (compatibility) |

**Realistic target after current v0.9 path:** strong batched throughput, but still constrained on single-job command workloads
**Realistic target after Phase C:** ~50,000-100,000 push/sec (hybrid memory/disk)

---

## Context: Why Other Backends Perform Better

| Backend | Write Model | Dequeue Model | Expected Throughput |
|---------|-------------|---------------|---------------------|
| redb | txn commits + index maintenance (single-job) / coalesced commits (batched) | Index prefix range scan | ~334/sec single-job TCP, ~10,929/sec batched TCP |
| SQLite (WAL) | fsync per txn, WAL batching | SQL indexes | ~2,000-5,000/sec |
| PostgreSQL | Shared buffer pool, WAL | B-tree indexes, SKIP LOCKED | ~10,000-30,000/sec |
| In-Memory | No I/O | HashMap lookup | ~100,000-500,000/sec |
| Redis | RDB snapshots, AOF | O(1) list operations | ~100,000+/sec |

The PRD's 50,000 push/sec target is achievable with the right storage strategy but **not with naive per-operation redb transactions**.

---

## Conclusion

The performance story now splits into two modes. Batched TCP mode has improved dramatically and can outperform several competitors in consume/end-to-end throughput on this local suite, while single-job command mode remains far below target. The next highest-impact step is automatic coalescing for single-job flows (so users get batching benefits without changing clients), followed by protocol and architecture upgrades for high-throughput modes.
