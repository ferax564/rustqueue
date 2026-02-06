# RustQueue Performance Analysis

**Date:** February 6, 2026
**Version:** v0.4 (Phase 4 complete)
**Status:** CRITICAL — throughput 147x below target

---

## Executive Summary

RustQueue's current throughput is ~340 push/sec with the default redb backend, versus the PRD target of 50,000 push/sec. The gap is 147x. The root causes are well-understood and addressable: per-operation fsync in redb, O(n) full table scans for dequeue, and no batch transaction support. This document details the bottlenecks, measured results, and a concrete optimization plan.

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

**Location:** `src/storage/redb.rs:55-67`

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

### 2. O(n) Full Table Scan for Dequeue — SECONDARY BOTTLENECK

**Location:** `src/storage/redb.rs:110-156`

The `dequeue()` method iterates **every job in the entire database** to find waiting jobs in a specific queue:

```rust
async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
    let write_txn = self.db.begin_write()?;
    let mut table = write_txn.open_table(JOBS_TABLE)?;

    // Scan ALL jobs — O(n) where n = total jobs across ALL queues
    let mut candidates: Vec<Job> = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        let job: Job = serde_json::from_slice(value.value())?;  // deserialize every job
        if job.queue == queue && job.state == JobState::Waiting {
            candidates.push(job);
        }
    }
    // ... sort and take count
}
```

With 100,000 jobs in the database, dequeue deserializes all 100,000 to find the handful that are waiting in the requested queue. This is O(n) in the total database size, not the queue size.

**Impact:** Dequeue latency grows linearly with total job count. At 100K jobs, this becomes the dominant bottleneck.

### 3. No Batch Transaction Support

**Location:** `src/storage/mod.rs:28` (StorageBackend trait)

The `StorageBackend` trait only exposes `insert_job(&self, job: &Job)` — one job at a time. There is no `batch_insert_jobs()` method. The HTTP batch push handler calls `push()` in a loop:

```rust
// In HTTP handler: each push is a separate transaction
for job_data in batch {
    manager.push(queue, name, data, opts).await?;
    // → storage.insert_job() → begin_write + commit + fsync
}
```

This means batch operations pay N × fsync cost instead of amortizing a single fsync across N inserts.

### 4. All Scan Operations Are O(n)

The same full-table-scan pattern appears in:

| Method | Location | Scans |
|--------|----------|-------|
| `dequeue()` | redb.rs:110 | All jobs, filter by queue+state |
| `get_queue_counts()` | redb.rs:158 | All jobs, filter by queue |
| `get_ready_scheduled()` | redb.rs:185 | All jobs, filter by state+delay_until |
| `get_dlq_jobs()` | redb.rs:224 | All jobs, filter by queue+state |
| `get_active_jobs()` | redb.rs:~340 | All jobs, filter by state |
| `remove_completed_before()` | redb.rs:244 | All jobs, filter by state+updated_at |
| `list_queue_names()` | redb.rs:~300 | All jobs, collect unique queues |

As the database grows, **every background scheduler tick** (which calls `get_ready_scheduled`, `get_active_jobs`, etc.) does multiple O(n) scans.

### 5. Blocking Async Runtime

**Location:** `src/storage/redb.rs:26-27`

```rust
/// All operations are synchronous under the hood; the async trait methods call
/// redb directly without `spawn_blocking` since redb operations are fast for v0.1.
```

redb operations are synchronous I/O. Calling them directly from async context blocks the tokio runtime thread. Under high concurrency, this starves other async tasks (HTTP handling, WebSocket events, TCP connections).

### 6. Single Flat Table Design

All jobs across all queues are stored in a single `JOBS_TABLE` keyed by UUID. There are no secondary indexes by queue name, state, priority, or timestamp. This makes any query other than "get job by ID" a full table scan.

---

## Comparison to Targets

| Metric | Current | Target | Gap | Priority |
|--------|---------|--------|-----|----------|
| Push throughput | ~340/sec | 50,000/sec | 147x | P0 |
| Push+ack throughput | ~117/sec | 30,000/sec | 256x | P0 |
| P50 push latency | 2.93 ms | < 1 ms | 2.9x | P1 |
| Dequeue at 100K jobs | O(n) scan | O(log n) | Degrades | P0 |
| Batch push 1000 | 6.44s | < 100ms | 64x | P0 |
| Binary size | 6.8 MB | < 15 MB | OK | - |
| Memory (idle) | ~15 MB | < 20 MB | OK | - |
| Startup | ~10 ms | < 500 ms | OK | - |

---

## Optimization Plan

### Phase A: Quick Wins (estimated 5-10x improvement)

#### A1. Batch Transaction API

Add `batch_insert_jobs(&self, jobs: &[Job])` to StorageBackend trait. Single write transaction + single fsync for N jobs.

```rust
async fn batch_insert_jobs(&self, jobs: &[Job]) -> Result<Vec<JobId>> {
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

**Expected impact:** Batch push/1000 goes from 6.44s → ~50-100ms (single fsync + N inserts in memory). That's 10,000-20,000 jobs/sec for batch operations.

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

#### A3. spawn_blocking for redb Operations

Wrap all redb calls in `tokio::task::spawn_blocking()` to prevent blocking the async runtime:

```rust
async fn insert_job(&self, job: &Job) -> Result<JobId> {
    let db = self.db.clone(); // redb::Database is Arc internally
    let job = job.clone();
    tokio::task::spawn_blocking(move || {
        // ... synchronous redb operations
    }).await?
}
```

**Expected impact:** Better concurrent throughput under load. Won't increase single-operation speed but prevents runtime starvation.

### Phase B: Index Optimization (estimated 10-50x improvement for queries)

#### B1. Secondary Index Tables in redb

Add auxiliary redb tables for common query patterns:

```rust
// Queue+State index: key = (queue, state, priority, created_at, job_id)
const QUEUE_STATE_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("idx_queue_state");

// State index: key = (state, updated_at, job_id)
const STATE_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("idx_state");
```

- `dequeue("emails", 10)` → range scan on QUEUE_STATE_INDEX for `("emails", "waiting", ...)` → O(log n + k) where k = result count
- `get_active_jobs()` → range scan on STATE_INDEX for `("active", ...)` → O(log n + k)
- `get_ready_scheduled()` → range scan on STATE_INDEX for `("delayed", ...)` then filter by delay_until

**Expected impact:** Dequeue goes from O(n) to O(log n). At 100K jobs, this is ~17 iterations instead of 100,000.

#### B2. Maintain Indexes in Same Transaction

All write operations (insert, update, delete) must update both the main table and index tables within the same transaction. This adds no extra fsync cost since they share the transaction.

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

| Priority | Optimization | Effort | Impact | Risk |
|----------|-------------|--------|--------|------|
| 1 | A1: Batch transaction API | Small | 10-20x for batches | Low |
| 2 | A3: spawn_blocking | Small | Better concurrency | Low |
| 3 | B1+B2: Secondary index tables | Medium | 10-50x for queries | Medium |
| 4 | A2: Write coalescing | Medium | 5-10x for singles | Medium (durability trade-off) |
| 5 | C1: Hybrid memory+disk | Large | 50-100x | High (durability trade-off) |
| 6 | C2: Per-queue sharding | Medium | 2-10x for dequeue | Low |
| 7 | D1: TCP pipelining | Medium | 2-5x for TCP | Low |
| 8 | C3: Lock-free memory | Small | 2-3x under contention | Low |
| 9 | D2: Binary protocol | Large | 2-5x | Medium (compatibility) |

**Realistic target after Phase A+B:** ~5,000-20,000 push/sec (redb with indexes + batching)
**Realistic target after Phase C:** ~50,000-100,000 push/sec (hybrid memory/disk)

---

## Context: Why Other Backends Perform Better

| Backend | Write Model | Dequeue Model | Expected Throughput |
|---------|-------------|---------------|---------------------|
| redb | fsync per txn | O(n) full scan | ~340/sec (current) |
| SQLite (WAL) | fsync per txn, WAL batching | SQL indexes | ~2,000-5,000/sec |
| PostgreSQL | Shared buffer pool, WAL | B-tree indexes, SKIP LOCKED | ~10,000-30,000/sec |
| In-Memory | No I/O | HashMap lookup | ~100,000-500,000/sec |
| Redis | RDB snapshots, AOF | O(1) list operations | ~100,000+/sec |

The PRD's 50,000 push/sec target is achievable with the right storage strategy but **not with naive per-operation redb transactions**.

---

## Conclusion

The performance gap is not a fundamental architectural flaw — it's an expected consequence of Phase 1-4 prioritizing correctness and feature completeness over throughput. The `StorageBackend` trait abstraction means optimizations can be implemented incrementally without changing the engine, API, or protocol layers. The optimization plan above provides a clear path from ~340/sec to the 50,000/sec target.
