# v0.3.0 — Production Embedded Release

**Date:** 2026-05-22
**Status:** Approved design, pre-implementation
**Positioning:** Embedded-first. "The SQLite of job queues" — durable background jobs inside a Rust app with no service to operate. This release makes embedded mode actually deliver on that promise.

## Problem

`RustQueue::redb(path).build()` constructs a `QueueManager` and stops (`src/builder.rs:119`). The background scheduler that runs all periodic housekeeping (`start_scheduler`, `src/engine/scheduler.rs:23`) is only wired up in server mode (`src/main.rs`). As a result, three features that v0.2.0 documents as working **silently do not fire in embedded/library mode**:

| Feature | Why it's broken embedded | Impact |
|---|---|---|
| Backoff retries | `fail()` routes a retry to `Delayed`; only `promote_delayed_jobs` brings it back, and that only runs in the scheduler tick | Retries with backoff stall forever |
| Schedules | `execute_schedules` only runs in the scheduler tick | Cron/interval schedules never fire |
| Crash recovery | `detect_stalls` re-queues jobs whose worker died; only runs in the scheduler tick | A `kill -9` job stays `Active` forever |

Immediate (no-backoff) retries do work, because the next `pull()` re-selects them. Everything that routes through `Delayed` does not.

This is a correctness gap in the shipped release, not just a missing convenience. Any "production embedded" documentation written today would describe broken behavior. The fix is therefore the foundation of this release.

A second, related gap: the documented worker pattern (`examples/worker.rs`) is a hand-rolled `loop { pull; for job { ack } }` with no retry-on-error, no graceful shutdown, and no backoff. It undercuts the "effortless background jobs" pitch.

## Goals

1. Make embedded mode correct: retries, schedules, and crash recovery fire without running the server.
2. Give library users an ergonomic worker entry point so they stop hand-rolling pull/ack/fail loops.
3. Ship the distribution assets that turn this into adoption signal: deployable example, operational docs, a crash-survival demo, website pages, and a v0.3.0 release.

## Non-goals (deferred to chunk A — Worker Runtime API)

- Configurable worker concurrency (this release is sequential, one job at a time).
- Tunable backoff/hooks on the worker builder.
- Worker-side heartbeat emission for long-running jobs.
- Panic isolation (`catch_unwind`) inside the worker loop.

Distributed/Raft mode (original ROADMAP P0) remains deferred indefinitely; embedded-first is the chosen direction.

## Design

### 1. Embedded housekeeping (the fix)

New public method on `RustQueue`:

```rust
/// Start the background housekeeping loop (delayed-job promotion, schedule
/// execution, timeout + stall detection, retention). Required for retries,
/// schedules, and crash recovery to work in embedded mode. Idempotent:
/// repeated calls return additional handles but spawn only one loop.
pub fn start_housekeeping(&self) -> HousekeepingHandle;
```

- Reuses `start_scheduler(manager, tick_interval_ms, stall_timeout_ms, retention)` unchanged.
- **Idempotency:** `QueueManager` gains an `AtomicBool` `housekeeping_started`. `start_housekeeping` uses `compare_exchange`; if already started it returns a no-op handle that does not abort the shared loop on drop. This prevents duplicate loops when a user calls `start_housekeeping` and also calls `run_worker` (which auto-ensures it).
- **`HousekeepingHandle`:** wraps the `JoinHandle`. Dropping the "owning" handle aborts the loop; no-op handles do not. Also exposes `stop()`.
- **Runtime:** `start_housekeeping` calls `tokio::spawn`, which requires a runtime. It is only reachable from async contexts in practice (the docs and `run_worker` guarantee this). `.build()` stays synchronous and grabs no runtime handle — avoiding the panic-outside-runtime footgun.

**Configuration (builder knobs, embedded defaults):**

```rust
RustQueue::redb("jobs.db")?
    .stall_timeout(Duration::from_secs(30))   // default 30s
    .tick_interval(Duration::from_secs(1))    // default 1s
    .build()?;
```

Stored on the builder, passed to `start_scheduler` when housekeeping starts. Retention uses the existing `RetentionConfig::default()`.

**Refactor:** `RustQueue` currently holds `manager: QueueManager`. The spawned housekeeping loop needs a shared owner, so this becomes `manager: Arc<QueueManager>`. All delegating methods (`push`, `pull`, `ack`, ...) keep the same bodies (`self.manager.foo()` works through the `Arc`). `start_scheduler` already takes `Arc<QueueManager>`, so it consumes a clone directly.

### 2. `run_worker` helper

```rust
/// Run a managed worker loop on `queue`: pulls jobs, runs `handler`, acks on
/// Ok and fails (engine applies retry/backoff/DLQ) on Err. Ensures housekeeping
/// is running. Stops on Ctrl-C. Sequential (one job at a time) in this release.
pub async fn run_worker<F, Fut, E>(&self, queue: &str, handler: F) -> Result<(), RustQueueError>
where
    F: Fn(Job) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display;

/// Same as `run_worker`, but stops when `shutdown` resolves instead of on
/// Ctrl-C. Use this when embedding in a web server (pass the server's
/// shutdown signal).
pub async fn run_worker_with_shutdown<F, Fut, E, S>(
    &self, queue: &str, handler: F, shutdown: S,
) -> Result<(), RustQueueError>
where /* ... */ S: Future<Output = ()>;
```

Loop behavior:

```text
ensure housekeeping running (idempotent)
loop {
    if shutdown signalled -> break
    jobs = pull(queue, 1)
    if jobs empty -> sleep(poll_interval=500ms); continue
    for job in jobs {
        match handler(job).await {
            Ok(())  => ack(job.id, None)
            Err(e)  => fail(job.id, &e.to_string())   // engine: retry w/ backoff or DLQ
        }
        if shutdown signalled -> break   // finish current job first
    }
}
return Ok(())
```

- `run_worker` selects over the loop body and a `tokio::signal::ctrl_c()` future.
- Panics in `handler` propagate and abort the worker task — documented; users should return `Err`. (Isolation deferred to chunk A.)

### 3. Enhanced Axum example

Upgrade `examples/axum_background_jobs.rs`:
- A realistic handler (simulated email send with latency + occasional failure to show retry).
- Worker spawned as a background task using `run_worker_with_shutdown`, wired to the same shutdown signal Axum's `with_graceful_shutdown` uses, so the worker drains on SIGTERM/Ctrl-C.
- Dashboard reachable; a `POST /enqueue` route that pushes a job.
- Top-of-file doc comment explaining the production shape.

### 4. Operational docs — `docs/production.md`

Sections:
- **Crash-only design:** state is persisted before `ack`; safe to `kill -9`. What "at-least-once" means here.
- **Durability modes:** table of redb (fsync per write, safest, ~314 push/s), buffered redb (batched, faster, small loss window on crash), hybrid (in-memory hot path, up to `snapshot_interval` loss). When to choose which.
- **Retries & DLQ:** `max_retries`, backoff strategies, dead-letter behavior.
- **Crash recovery:** an `Active` job whose worker dies is re-queued by stall detection after `stall_timeout`. Requires housekeeping running.
- **Housekeeping:** what it does, that it is required, how it starts (`run_worker` auto-ensures, or `start_housekeeping` for manual-pull users), and how to tune `stall_timeout`/`tick_interval`.
- **Graceful shutdown:** use `run_worker_with_shutdown`; drains in-flight job.

### 5. Killer demo — `examples/crash_recovery.rs` + walkthrough

- Pushes a slow job (handler sleeps ~10s, prints progress).
- `run_worker` picks it up; configured with a short `stall_timeout` (e.g. 3s) for a snappy demo.
- Walkthrough doc (in `docs/production.md` and README): exact commands — run the example, `kill -9` it mid-job, re-run it, observe the job re-queued and completed. Includes expected console output.

### 6. README + website

- README: a "Production" section with the `run_worker` snippet and the crash-survival pitch, linking to `docs/production.md`.
- GitHub Pages: a self-contained "Production guide" HTML page (inline CSS, no external stylesheet refs, per the Pages constraint), plus links from the landing/examples pages.
- Embedded dashboard static pages (`dashboard/static/`) updated to match.

### 7. Release v0.3.0

- Bump version in all five locations: `Cargo.toml`, `sdk/node/package.json`, `sdk/python/pyproject.toml`, `sdk/python/rustqueue/__init__.py`, `src/api/openapi.rs`.
- CHANGELOG entry framing the housekeeping change as a **bug fix** ("embedded retries, schedules, and crash recovery now work without running the server") plus the new `run_worker`/`start_housekeeping` API and docs. Not a breaking change.
- Tag `v0.3.0`; CI publishes to crates.io. Use the manual tag push sequence (GITHUB_TOKEN-created tags can't trigger the publish workflow).

## Data flow

```
push ─────────────► storage: Waiting
                         │
   housekeeping tick ────┼─► promote_delayed (retries) ─► Waiting
   (start_housekeeping)  ├─► execute_schedules ─► push new jobs
                         └─► detect_stalls ─► re-queue Active-but-dead ─► Waiting
                         │
run_worker: pull ──► Active ──► handler ──► Ok: ack (done)
                                       └──► Err: fail ──► retry (Delayed/Waiting) or DLQ
                         │
crash (kill -9): job left Active, no heartbeat ──► detect_stalls re-queues after stall_timeout
```

## Testing

Regression tests proving the fix **with no server running** (pure embedded `RustQueue`):
- A failed job with backoff is promoted back to `Waiting` and retried after housekeeping ticks.
- A due schedule fires and pushes a job.
- A job left `Active` past `stall_timeout` is re-queued.

Worker tests:
- `run_worker` acks on `Ok`, calls `fail` on `Err`.
- `run_worker_with_shutdown` returns when the shutdown future resolves, after finishing the in-flight job.
- `start_housekeeping` called twice spawns one loop (idempotency).

Crash-recovery integration test: simulate a worker dropping mid-`Active` (no ack), confirm re-pull after `stall_timeout`.

Doc/example tests: `examples/crash_recovery.rs` and the enhanced Axum example compile; doctests for `run_worker`/`start_housekeeping` compile.

## Risks & mitigations

- **`Arc<QueueManager>` refactor** touches `src/builder.rs` delegations and any constructor callers — mechanical, covered by the existing suite.
- **`stall_timeout` too short** re-queues a slow job that simply hasn't acked yet (no heartbeat in this release). Mitigation: conservative 30s default; document that long-running jobs need a longer timeout (worker-side heartbeat lands in chunk A).
- **Behavior change** (backoff retries/schedules now fire embedded) could surprise an embedded user who relied on the broken behavior. Extremely unlikely; framed as a bug fix in CHANGELOG.
- **Duplicate housekeeping** if a user calls both `start_housekeeping` and `run_worker` — handled by the idempotency guard.

## Out of scope / follow-on chunks

- **Chunk A — Worker Runtime API:** concurrency, backoff config, lifecycle hooks, worker-side heartbeat, panic isolation.
- **Chunk B — Lifecycle guarantees:** formal lease/visibility-timeout model, at-least-once proofs under partition, atomic check-then-insert idempotency.
