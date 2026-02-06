# RustQueue Competitor Gap Analysis (2026-02-06)

## Current Best Profile (TCP Batch Size = 50)

Source: `docs/competitor-benchmark-2026-02-06.json` (`--ops 500 --repeats 2 --redb-durability immediate --rustqueue-tcp-batch-size 50`)

| System | Produce ops/s | Consume ops/s | End-to-end ops/s |
|--------|---------------:|--------------:|-----------------:|
| rabbitmq | 27,637 | 1,922 | 1,519 |
| rustqueue_tcp | 10,929 | 5,970 | 3,692 |
| redis_list | 3,005 | 3,448 | 1,704 |
| bullmq | 1,728 | 1,804 | 545 |
| celery | 983 | 513 | 342 |
| rustqueue_http | 245 | 105 | 73 |

RustQueue TCP relative position in this profile:

- Produce: `~2.5x` behind fastest (`rabbitmq`)
- Consume: **winner** (`~1.7x` faster than `redis_list`)
- End-to-end: **winner** (`~2.2x` faster than `redis_list`, `~2.4x` faster than `rabbitmq`)

## Single-Job Control (TCP Batch Size = 1)

Source snapshot: `docs/competitor-benchmark-2026-02-06-immediate-r2-latest.json`

- rustqueue_tcp: produce `334`, consume `73`, end-to-end `56`

Batching/coalescing uplift (TCP):

- Produce: `334 -> 10,929` (`~32.7x`)
- Consume: `73 -> 5,970` (`~81.8x`)
- End-to-end: `56 -> 3,692` (`~65.9x`)

## What Was Implemented

### 1. Storage-level batch completion API (write coalescing primitive)

- Added `complete_jobs_batch()` to storage trait: `src/storage/mod.rs`
- Added redb transactional override for batch completion: `src/storage/redb.rs`

This coalesces many `ack` completions into one write transaction/commit.

### 2. QueueManager batch ack path

- Added `ack_batch()` with per-item results: `src/engine/queue.rs`

This uses storage batch completion in one backend call and emits completion events per successful job.

### 3. TCP protocol batch commands

- Added `push_batch`: `src/protocol/handler.rs`
- Added `ack_batch`: `src/protocol/handler.rs`

These reduce protocol round-trips and unlock storage coalescing from clients.

### 4. Benchmark harness support

- Added `--rustqueue-tcp-batch-size`: `scripts/benchmark_competitors.py`

When `>1`, TCP benchmark uses `push_batch`, `pull count=N`, and `ack_batch`.

## Why We Still Lag in Some Cases

### 1. Single-job path is still expensive

Without batching, lifecycle writes still commit frequently:

- insert write: `src/storage/redb.rs:295`
- dequeue transition write: `src/storage/redb.rs:482`
- completion write: `src/storage/redb.rs:425`

### 2. Transition index churn remains

Each state transition updates main row plus secondary index rows.

### 3. Batch mode is opt-in

Clients must send `push_batch`/`ack_batch` (or equivalent high-count pull) to get this throughput tier.

## Practical Conclusion

- RustQueue is now competitive in local throughput when clients use batched TCP commands.
- Single-job command throughput is still far below long-term targets.
- The next highest-impact step is automatic coalescing for single-job flows (server-side buffering/flush policy) so clients benefit without protocol changes.
