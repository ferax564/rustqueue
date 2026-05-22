# v0.3.0 — Production Embedded Release

**Date:** 2026-05-22
**Status:** Approved design (revised after Codex code-verified review), pre-implementation
**Positioning:** Embedded-first. "The SQLite of job queues" — durable background jobs inside a Rust app with no service to operate. This release makes embedded mode actually deliver on that promise.

## Problem

`RustQueue::redb(path).build()` constructs a `QueueManager` and stops (`src/builder.rs:118-132`). `RustQueue` stores `manager: QueueManager` (a value, not an `Arc`; `src/builder.rs:35-37`). The background scheduler that runs all periodic housekeeping (`start_scheduler`, `src/engine/scheduler.rs:23-107`) is only wired up in server mode (`src/main.rs:439-445`). As a result, features that v0.2.0 implies work in embedded mode do not fire:

| Feature | Why it's broken embedded | Impact |
|---|---|---|
| Backoff retries | `fail()` routes a retry to `Delayed` (`queue.rs:764-805`); only `promote_delayed_jobs` brings it back, and that only runs in the scheduler tick | Retries with backoff stall forever |
| Crash recovery | `detect_stalls()` (`queue.rs:959-989`) reclaims a job whose worker died by calling `fail()`, which then routes to Delayed/Waiting/DLQ by attempt count; only runs in the scheduler tick | A `kill -9` job stays `Active` forever |
| Schedules | `execute_schedules` only runs in the scheduler tick **and** `RustQueue` does not expose schedule CRUD at all (delegations end at `update_progress`, `builder.rs:138-218`) | Embedded users can neither create nor fire schedules |

Note (corrected from first draft): immediate, no-backoff retries do work, because the next `pull()` re-selects a `Waiting` job. The default retry is delayed 1000ms (`models.rs:137-140`), so it goes through `Delayed` and stalls. "Crash recovery" is not a direct re-queue — it is `detect_stalls` → `fail` → the normal retry/DLQ path.

A second gap: the documented worker pattern (`examples/worker.rs`) is a hand-rolled `loop { pull; for job { ack } }` with no retry-on-error, no graceful shutdown, no backoff.

This is a correctness gap in the shipped release. Any "production embedded" documentation written today would describe broken behavior. The housekeeping fix is the foundation of this release.

## Goals

1. Make embedded mode correct: retries, crash recovery, and schedules fire without running the server.
2. Give library users an ergonomic, **safe** worker entry point (`run_worker`) so they stop hand-rolling loops.
3. Ship the distribution assets that turn this into adoption signal: deployable example, operational docs, a crash-survival demo, website pages, and a v0.3.0 release.

## Non-goals (deferred to chunk A — Worker Runtime API)

- Configurable worker **concurrency** (v0.3.0 is sequential, one job at a time).
- Tunable backoff / lifecycle hooks on a worker builder.
- Panic isolation (`catch_unwind`) inside the worker loop.
- Configurable heartbeat cadence and a standalone heartbeat API. (v0.3.0 ships a *minimal automatic* heartbeat inside `run_worker` only — see §2.)

Distributed/Raft mode (original ROADMAP P0) remains deferred indefinitely; embedded-first is the chosen direction.

## Design

### 1. Embedded housekeeping (the fix)

`RustQueue` gains an internal, ref-counted housekeeping owner. **No `Drop`-guard handle is returned** (the first draft's drop-aborts-loop design was a footgun: an unused return value would abort the scheduler immediately, and auto-start from `run_worker` would abort on handle discard).

```rust
struct HousekeepingState {
    started: AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,   // aborted on Drop of the last Arc
}

pub struct RustQueue {
    manager: Arc<QueueManager>,
    housekeeping: Arc<HousekeepingState>,
}
```

- **Lifetime = `RustQueue` lifetime.** `RustQueue` is `Clone` (both fields are `Arc`). The housekeeping loop runs until the last clone drops; `Drop for HousekeepingState` aborts the stored `JoinHandle` when the refcount hits zero. There is no per-call guard to forget.
- **`pub fn start_housekeeping(&self) -> Result<(), RustQueueError>`** — idempotent. Uses `tokio::runtime::Handle::try_current()`; returns `Err` (not panic) if called outside a runtime. Then `started.compare_exchange(false, true)`: the winner spawns `start_scheduler(...)` and stores the `JoinHandle`; losers return `Ok(())` without spawning. The flag is set only **after** a successful spawn, so a failed spawn cannot poison it.
- **Restart is not supported in v0.3.0** (documented). Clones share one loop; explicit restart lands later if needed.
- Reuses `start_scheduler(manager, tick_interval_ms, stall_timeout_ms, retention)` unchanged.

**Configuration (builder knobs, embedded defaults):**

```rust
RustQueue::redb("jobs.db")?
    .stall_timeout(Duration::from_secs(30))   // default 30s
    .tick_interval(Duration::from_secs(1))    // default 1s
    .build()?;
```

- Stored on the builder, passed to `start_scheduler` when housekeeping starts. Retention uses `RetentionConfig::default()`.
- **Validation:** reject `Duration::ZERO` for `tick_interval` (a zero `tokio::time::interval` busy-loops) and clamp to a ≥1ms floor. `build()` returns `Err` on invalid config.

**Refactor scope:** `manager` becomes `Arc<QueueManager>`; delegating bodies are unchanged. Derive/implement `Clone` for `RustQueue`. Keep housekeeping state on `RustQueue`, **not** in the core `QueueManager` (engine stays free of embedded-only state). Server mode already uses `Arc<QueueManager>` (`main.rs:380-385`) and `start_scheduler` already takes `Arc<QueueManager>` (`scheduler.rs:23-28`).

**New embedded API surface on `RustQueue`** (required for the schedule + heartbeat claims to be real): `create_schedule`, `list_schedules`, `pause_schedule`, `resume_schedule`, `delete_schedule`, `heartbeat`, `get_dlq_jobs`. All delegate to existing `QueueManager` methods.

### 2. `run_worker` helper

```rust
pub async fn run_worker<F, Fut, E>(&self, queue: &str, handler: F) -> Result<(), RustQueueError>
where F: Fn(Job) -> Fut, Fut: Future<Output = Result<(), E>>, E: std::fmt::Display;

pub async fn run_worker_with_shutdown<F, Fut, E, S>(
    &self, queue: &str, handler: F, shutdown: S,
) -> Result<(), RustQueueError>
where /* ...same... */ S: Future<Output = ()>;
```

Corrected loop (fixes move-after-use, mid-job cancellation, and unsafe long jobs):

```text
start_housekeeping()  (idempotent; propagate Err)
loop {
    if shutdown signalled -> break               // checked only here, while idle
    jobs = pull(queue, 1)
    if jobs empty -> select { shutdown => break, sleep(poll_interval=500ms) => continue }
    for job in jobs {
        let job_id = job.id;                     // capture BEFORE moving job into handler
        let hb = spawn_heartbeat(job_id);        // pings heartbeat every stall_timeout/3 (floor 1s)
        let outcome = handler(job).await;        // NOT wrapped in the shutdown select
        hb.abort();                              // stop heartbeating this job
        match outcome {
            Ok(())  => if let Err(e) = ack(job_id, None) { warn!(?e); }     // log & continue
            Err(err) => {
                let msg = truncate(err.to_string(), MAX_ERR=8KiB);          // < 10KiB storage cap
                if let Err(e) = fail(job_id, &msg) { warn!(?e); }            // log & continue
            }
        }
        if shutdown signalled -> break           // finish the in-flight job first, then stop
    }
}
return Ok(())
```

- **Shutdown** is only observed while idle / between jobs, never around `handler.await` — so Ctrl-C / the shutdown future never drops a running job and leaves it `Active`.
- **Automatic heartbeat** (pulled forward from chunk A): while the handler runs, a side task calls `self.heartbeat(job_id)` every `stall_timeout/3`. This keeps `detect_stalls` (`last_heartbeat.or(started_at)`, `queue.rs:969-974`) from reclaiming a healthy long-running job. On `kill -9` the whole process dies, the heartbeat stops, and the job is reclaimed after `stall_timeout`. Without this, `run_worker` would corrupt any job running longer than `stall_timeout`.
- **`ack`/`fail` failures are logged and the loop continues** (e.g. if housekeeping already stalled the job, `ack` errors at `queue.rs:622-645`). The worker does not exit on a single bookkeeping error.
- **Error truncation:** worker error strings are truncated below the storage cap (`queue.rs:742-748`) so `fail()` itself can't fail and strand the job `Active`.
- Sequential `pull(queue, 1)`. This is **minimum-viable correctness, not a throughput runtime** — documented as such; concurrency is chunk A.
- **Panics:** a panic in `handler` aborts the worker task; under `panic = abort` it ends the process and the job stays `Active` until stall detection reclaims it. Documented explicitly; users should return `Err`.

### 3. Enhanced Axum example

Upgrade `examples/axum_background_jobs.rs`: a realistic handler (simulated email send with latency + occasional failure to exercise retry); the worker spawned via `tokio::spawn` of a cloned `RustQueue` running `run_worker_with_shutdown`, wired to the same shutdown signal Axum's `with_graceful_shutdown` uses; a `POST /enqueue` route; dashboard reachable. Top-of-file doc comment explains the production shape.

### 4. Operational docs — `docs/production.md`

Crash-only design + `kill -9` safety; durability-modes table (redb fsync ~314/s safest / buffered batched / hybrid up-to-`snapshot_interval`-loss — when to use which); retries + backoff + DLQ; crash recovery via stall detection (requires housekeeping); housekeeping (what, that it's required, `run_worker` auto-ensures it or call `start_housekeeping`, tuning `stall_timeout`/`tick_interval`); heartbeat behavior and the long-job/`stall_timeout` relationship; graceful shutdown via `run_worker_with_shutdown`.

### 5. Killer demo — `examples/crash_recovery.rs` + walkthrough

- Pushes a slow job (handler sleeps ~10s, prints progress). `run_worker` heartbeats it while alive, so a healthy worker is **not** falsely reclaimed (this is why heartbeat must be in v0.3.0 — the first draft's 10s-job/3s-stall demo would have marked a healthy worker dead).
- Walkthrough (in `docs/production.md` + README): run the example, `kill -9` mid-job (heartbeat stops), re-run it; after `stall_timeout` (set short, e.g. 3s, for the demo) the job is reclaimed and completes. Includes expected console output.

### 6. README + website

README "Production" section (`run_worker` snippet + crash-survival pitch, link to `docs/production.md`). GitHub Pages: a self-contained "Production guide" HTML page (inline CSS, no external stylesheet refs, per the Pages constraint) linked from landing/examples. Embedded dashboard static pages (`dashboard/static/`) updated to match. (Kept in v0.3.0 per owner decision; Codex recommended deferring — owner chose the single combined adoption push.)

### 7. Release v0.3.0

Bump version in all five locations: `Cargo.toml`, `sdk/node/package.json`, `sdk/python/pyproject.toml`, `sdk/python/rustqueue/__init__.py`, `src/api/openapi.rs`. CHANGELOG frames the housekeeping change as a **bug fix** ("embedded retries and crash recovery now work without running the server; embedded schedule API added") plus `run_worker`/`start_housekeeping`. Not a breaking change. Tag `v0.3.0`; CI publishes. Use the manual tag-push sequence (GITHUB_TOKEN-created tags can't trigger the publish workflow).

## Data flow

```
push ─────────────► storage: Waiting
                         │
   housekeeping ─────────┼─► promote_delayed (retries) ─► Waiting
   (start_housekeeping,  ├─► execute_schedules ─► push new jobs
    auto via run_worker) └─► detect_stalls ─► fail() reclaim ─► Waiting/Delayed/DLQ
                         │
run_worker: pull ─► Active ─► [heartbeat ticks] ─► handler ─► Ok: ack
                                                          └─► Err: fail ─► retry or DLQ
                         │
crash (kill -9): job Active, heartbeat stops ─► detect_stalls reclaims after stall_timeout
```

## Testing

Regression tests proving the fix **with no server running** (pure embedded `RustQueue`):
- A failed job with backoff is promoted back and retried after housekeeping ticks.
- A due schedule (created via the new embedded API) fires and pushes a job.
- A job left `Active` past `stall_timeout` is reclaimed.

Worker + lifecycle tests (the new race-prone areas Codex flagged):
- `run_worker` acks on `Ok`, calls `fail` on `Err`.
- `run_worker_with_shutdown` returns when shutdown resolves, **after** finishing the in-flight job (assert the job is not left `Active`).
- A handler running longer than `stall_timeout` is **not** reclaimed (heartbeat works).
- `fail()` with an over-long error string does not strand the job (truncation works).
- `ack()` failing because housekeeping already moved the job → worker logs and continues.
- `start_housekeeping` called twice spawns one loop; a second cloned `RustQueue` running a worker does not spawn a second loop.
- Dropping all `RustQueue` clones aborts the housekeeping task (no leaked task).
- `start_housekeeping` outside a runtime returns `Err`, not panic.
- `tick_interval(Duration::ZERO)` is rejected by `build()`.

Doc/example tests: `examples/crash_recovery.rs` and the enhanced Axum example compile; doctests for `run_worker`/`start_housekeeping` compile.

## Risks & mitigations

- **`Arc<QueueManager>` + `Clone` refactor** touches `builder.rs` delegations; mechanical, covered by the suite.
- **Housekeeping lifetime via `Drop`** — verify the `Drop` aborts the task and there is no `Arc` cycle (`HousekeepingState` must not hold a strong ref back to `RustQueue`).
- **`stall_timeout` vs job duration** — heartbeat covers in-flight jobs; a job that *blocks the async executor* (CPU-bound, no `.await`) can't heartbeat and may be reclaimed. Document: keep handlers async-friendly, or raise `stall_timeout`.
- **Behavior change** (backoff retries / crash recovery now fire embedded) — framed as a bug fix in CHANGELOG; extremely unlikely anyone relied on the broken behavior.

## Out of scope / follow-on chunks

- **Chunk A — Worker Runtime API:** concurrency, backoff config, lifecycle hooks, configurable/standalone heartbeat, panic isolation.
- **Chunk B — Lifecycle guarantees:** formal lease/visibility-timeout model, at-least-once proofs under partition, atomic check-then-insert idempotency.
- **v0.3.1+ candidates:** housekeeping restart semantics.
