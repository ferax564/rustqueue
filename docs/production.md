# RustQueue in Production

This guide covers everything you need to run RustQueue reliably in an embedded deployment: durability tradeoffs, managed workers, retries, crash recovery, and graceful shutdown.

---

## 1. Crash-only design

RustQueue is built around a crash-only principle: **every job is persisted to storage before it is acknowledged**. There is no in-memory state that would be lost on a crash. You can `kill -9` the process at any point and no committed work will be silently dropped.

Delivery is **at-least-once**: a job that was being processed when the process died will be retried (see [Crash recovery](#5-crash-recovery)). Your handlers should be idempotent, or you should use the `unique_key` field to deduplicate on re-delivery.

---

## 2. Durability modes

Choose the backend that matches your availability/throughput tradeoff:

| Backend | Durability | Throughput | Crash loss window | When to use |
|---------|-----------|------------|-------------------|-------------|
| **`redb`** (default) | ACID, fsync per write | ~314 push/s raw | None — zero data loss | Default production choice. Correct and zero-config. |
| **Buffered redb** (`.with_write_coalescing`) | Batched fsync | ~19 K ops/s | One flush interval | High-write workloads where small loss windows are acceptable. |
| **Hybrid** (`RustQueue::hybrid`) | In-memory hot path + periodic snapshot | 300 K+ ops/s | Up to `snapshot_interval` | Maximum throughput; explicitly trade durability for speed. |
| **In-memory** (`RustQueue::memory`) | None — lost on drop | Fastest | Total | Tests and ephemeral workloads only. |

```rust
// Safest: ACID redb
let rq = RustQueue::redb("./jobs.db")?.build()?;

// Faster writes, tiny loss window:
use rustqueue::storage::BufferedRedbConfig;
let rq = RustQueue::redb("./jobs.db")?
    .with_write_coalescing(BufferedRedbConfig::default())
    .build()?;

// Maximum throughput, explicit durability tradeoff:
let rq = RustQueue::hybrid("./jobs.db")?.build()?;
```

---

## 3. Workers

`run_worker` is the managed entry point for processing jobs. It replaces a hand-rolled `pull` / `ack` / `fail` loop and handles the operational details automatically:

- **Auto-acks** a job when the handler returns `Ok(())`.
- **Auto-fails** a job (triggering engine retry/DLQ logic) when the handler returns `Err(e)`.
- **Auto-heartbeats** the in-flight job so stall detection does not reclaim a healthy long-running job.
- **Starts housekeeping** automatically on first call (no separate setup required).
- **Sequential**: processes one job at a time. This is intentional — minimum-viable correctness. Concurrent workers arrive in a later release.

```rust
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("./jobs.db")?.build()?;

    rq.run_worker("emails", |job| async move {
        println!("sending email: {}", job.data["to"]);
        // return Ok to ack, Err to fail (→ retry or DLQ)
        Ok::<(), String>(())
    })
    .await?;

    Ok(())
}
```

`run_worker` runs until Ctrl-C. It finishes the in-flight job before returning.

### Embedding in a web server

When you need to stop the worker when a server shuts down rather than on Ctrl-C, use `run_worker_with_shutdown` and pass your server's graceful-shutdown signal:

```rust
use tokio::sync::oneshot;

let (tx, rx) = oneshot::channel::<()>();

// In your server shutdown handler: tx.send(()).ok();

tokio::spawn(async move {
    rq.run_worker_with_shutdown("emails", handler, async {
        rx.await.ok();
    })
    .await
    .expect("worker failed");
});
```

The in-flight job is always completed before the worker returns, whether the shutdown comes from Ctrl-C or a custom signal.

---

## 4. Retries and DLQ

Every job carries retry configuration. The defaults are:

| Setting | Default |
|---------|---------|
| `max_attempts` | 3 |
| `backoff` | Exponential |
| `backoff_delay_ms` | 1 000 ms |

When a job fails (handler returns `Err`, or the worker crashes and stall detection fires), the engine applies the backoff and schedules a retry — unless `attempt >= max_attempts`, at which point the job moves to the dead-letter queue (DLQ).

**Backoff formulas** (where `base = backoff_delay_ms`, `attempt` starts at 1 after the first failure):

| Strategy | Delay formula | Example (base=1 s) |
|----------|--------------|---------------------|
| `Fixed` | `base` | 1 s, 1 s, 1 s |
| `Linear` | `base × attempt` | 1 s, 2 s, 3 s |
| `Exponential` (default) | `base × 2^(attempt-1)` | 1 s, 2 s, 4 s |

To override retry settings when pushing a job:

```rust
use rustqueue::engine::queue::JobOptions;
use rustqueue::engine::models::BackoffStrategy;

rq.push("emails", "send-welcome", data, Some(JobOptions {
    max_attempts: Some(5),
    backoff_delay_ms: Some(2_000),
    // BackoffStrategy is set on the job struct; push via serde or a typed helper
    ..Default::default()
})).await?;
```

**Inspecting the DLQ:**

```rust
let dead = rq.get_dlq_jobs("emails", 50).await?;
for job in dead {
    println!("{}: {:?}", job.id, job.last_error);
}
```

---

## 5. Crash recovery

When a worker process dies mid-job, the job stays in `Active` state with no heartbeat. Housekeeping's stall detection (`detect_stalls`) periodically scans active jobs and reclaims any that have not sent a heartbeat within `stall_timeout` (default: 30 s). Reclaiming calls `fail()` internally, so:

- If the job has retries remaining it is re-queued with backoff.
- If `attempt >= max_attempts` the job goes to the DLQ.

**Important:** this means a job that crashes on every attempt will exhaust its retries and land in the DLQ after `max_attempts` stall cycles. With the default of 3 attempts, a job that is killed three times ends up in the DLQ. Increase `max_attempts` for workloads that may experience frequent infrastructure interruptions.

**Housekeeping must be running** for stall detection to fire. `run_worker` starts it automatically. If you pull/ack manually, call `start_housekeeping()` explicitly — see [Housekeeping](#6-housekeeping).

---

## 6. Housekeeping

The housekeeping loop is a background tokio task that runs on every `tick_interval` (default: 1 s) and performs:

1. **Delayed job promotion** — moves jobs from `Delayed` to `Waiting` when their delay has expired.
2. **Schedule execution** — fires cron and interval schedules that are due.
3. **Timeout detection** — fails jobs that have exceeded their `timeout_ms`.
4. **Stall detection** — fails `Active` jobs that have not heartbeated within `stall_timeout`.
5. **Metrics update** — refreshes queue depth gauges every tick.
6. **Orphaned blocked jobs** — promotes DAG jobs whose parents completed but were missed (runs every 10 ticks).
7. **Retention cleanup** — removes expired completed/failed/DLQ jobs (runs every 60 ticks, ~1 minute at 1 s tick).

**Housekeeping is required for retries, schedules, and crash recovery to work.** If you omit it, delayed jobs never become runnable, stalled jobs stay stuck forever, and schedules never fire.

### Starting housekeeping

`run_worker` calls `start_housekeeping()` automatically. If you are managing `pull`/`ack` yourself:

```rust
// Call once after building; safe to call multiple times (idempotent).
rq.start_housekeeping()?;
```

`start_housekeeping` returns `Err` if called outside a Tokio runtime. The housekeeping task is automatically aborted when the last `RustQueue` clone is dropped.

### Tuning

```rust
use std::time::Duration;

let rq = RustQueue::redb("./jobs.db")?
    .stall_timeout(Duration::from_secs(60))   // default: 30s
    .tick_interval(Duration::from_millis(500)) // default: 1s
    .build()?;
```

- **`stall_timeout`**: raise it for workloads with legitimately long jobs (see [Heartbeats and long jobs](#7-heartbeats-and-long-jobs)).
- **`tick_interval`**: lower it for faster delayed/schedule dispatch; raise it to reduce storage I/O on very constrained hardware.

---

## 7. Heartbeats and long jobs

`run_worker` automatically sends a heartbeat at approximately `(stall_timeout / 2).clamp(200ms, 30s)`. For the default 30 s stall timeout, heartbeats fire every 15 s.

This keeps legitimately long-running jobs (minutes of async work) from being falsely reclaimed as stalled. As long as the handler stays running and yields to the async executor, the heartbeat fires and the job stays safe.

**CPU-bound handlers are the exception.** A handler that never `.await`s will block the tokio thread and prevent the heartbeat task from running. After `stall_timeout`, housekeeping will reclaim the job as stalled even though the process is alive. To prevent this:

- Prefer async-friendly work (`tokio::time::sleep`, network calls, async I/O).
- Wrap CPU-intensive sections with `tokio::task::spawn_blocking`.
- Or raise `stall_timeout` to give the CPU-bound work enough time to complete.

If you are using `pull`/`ack` directly (no `run_worker`), send heartbeats manually:

```rust
// Somewhere in your worker loop while the job is active:
rq.heartbeat(job.id).await?;
```

---

## 8. Graceful shutdown

For process-level shutdown (SIGTERM, container stop), use `run_worker` — it catches Ctrl-C and finishes the in-flight job before returning:

```rust
// Blocks until Ctrl-C, finishes current job first.
rq.run_worker("work", handler).await?;
```

For programmatic shutdown (e.g., from an Axum shutdown signal):

```rust
use tokio::sync::watch;

let (shutdown_tx, shutdown_rx) = watch::channel(false);

// Pass a future that resolves when the server shuts down:
rq.run_worker_with_shutdown("work", handler, async move {
    shutdown_rx.changed().await.ok();
})
.await?;

// Elsewhere, to trigger shutdown:
shutdown_tx.send(true).ok();
```

The worker always completes the job it is currently processing before stopping. Jobs that have not yet been pulled remain in `Waiting` state and will be picked up on the next startup.

---

## 9. Crash-recovery walkthrough

The `crash_recovery` example demonstrates the full lifecycle: push a job, crash mid-processing, restart, and watch the job complete automatically.

### Setup

The example uses a short `stall_timeout` (3 s) so you don't have to wait long:

```rust
let rq = RustQueue::redb("/tmp/rustqueue-crash-demo.db")?
    .stall_timeout(Duration::from_secs(3))
    .tick_interval(Duration::from_millis(500))
    .build()?;
```

### Step 1: Start the worker (Terminal 1)

```bash
cargo run --example crash_recovery -- worker
```

Expected output:
```
worker pid 12345 — process a slow job, then `kill -9` me mid-job
```

### Step 2: Push a job (Terminal 2)

```bash
cargo run --example crash_recovery -- push
```

Expected output:
```
pushed job 0196f3b2-...
```

Back in Terminal 1, the worker picks it up:
```
[start] 0196f3b2-... {"n":1}
[work ] 0196f3b2-... step 1/10
[work ] 0196f3b2-... step 2/10
...
```

### Step 3: Simulate a crash

While the job is running, kill the worker from Terminal 2 (substitute the PID printed at startup):

```bash
kill -9 12345
```

The job is now `Active` with no heartbeat. The database still holds it.

### Step 4: Restart the worker (Terminal 1)

```bash
cargo run --example crash_recovery -- worker
```

After ~3 seconds (the `stall_timeout`), housekeeping detects no heartbeat, calls `fail()`, and re-queues the job. The new worker instance picks it up:

```
worker pid 67890 — process a slow job, then `kill -9` me mid-job
[start] 0196f3b2-... {"n":1}
[work ] 0196f3b2-... step 1/10
...
[work ] 0196f3b2-... step 10/10
[done ] 0196f3b2-...
```

The job ran to completion with no manual intervention. No data loss, no message broker.

> **Note:** this walkthrough consumes one retry attempt per crash. With `max_attempts=3` (the default), a job that is killed three times will land in the DLQ instead of retrying. Raise `max_attempts` or lower `stall_timeout` for workloads that need faster or more tolerant recovery.
