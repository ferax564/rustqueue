# v0.3.0 Production Embedded — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make RustQueue's embedded mode correct (retries, crash recovery, schedules fire without the server), add an ergonomic + safe `run_worker` entry point, and ship it as v0.3.0 with docs, examples, a crash-survival demo, and website.

**Architecture:** `RustQueue` becomes a cheaply-cloneable handle over `Arc<QueueManager>` plus an `Arc<HousekeepingState>` whose `Drop` aborts the background scheduler when the last clone drops. `run_worker` drives a sequential pull/handle/ack-or-fail loop, auto-starts housekeeping, and heartbeats the in-flight job so stall detection never reclaims a healthy worker. The existing `start_scheduler` engine loop is reused unchanged.

**Tech Stack:** Rust, tokio (async + `signal` feature), redb (default storage), existing `QueueManager`/`start_scheduler`.

**Spec:** `docs/superpowers/specs/2026-05-22-production-embedded-design.md`

**Branch:** `feat/v0.3.0-production-embedded` (already created).

---

## File structure

| File | Responsibility | Action |
|------|----------------|--------|
| `src/housekeeping.rs` | `HousekeepingState` (started flag + JoinHandle, aborts on Drop) | Create |
| `src/builder.rs` | `RustQueue` struct (Arc fields, Clone), builder knobs + validation, `start_housekeeping`, schedule/heartbeat/dlq delegations | Modify |
| `src/worker.rs` | `run_worker` / `run_worker_with_shutdown` (`impl RustQueue`), error truncation helper | Create |
| `src/lib.rs` | `mod housekeeping; mod worker;` | Modify |
| `Cargo.toml` | tokio `signal` feature; version bump (Task 13) | Modify |
| `examples/axum_background_jobs.rs` | Production-shaped Axum example | Modify |
| `examples/crash_recovery.rs` | Crash-survival demo | Create |
| `docs/production.md` | Operational guide | Create |
| `README.md` | Production section | Modify |
| `docs/*.html`, `dashboard/static/*` | Website production guide | Modify |
| `CHANGELOG.md`, version files | Release | Modify |

Reused engine signatures (do not change them):
- `QueueManager::new(storage: Arc<dyn StorageBackend>) -> QueueManager`
- `start_scheduler(manager: Arc<QueueManager>, tick_interval_ms: u64, stall_timeout_ms: u64, retention: RetentionConfig) -> JoinHandle<()>` (`src/engine/scheduler.rs:23`)
- `QueueManager::{ack(id, Option<Value>), fail(id, &str), heartbeat(id), create_schedule(&Schedule), get_schedule(&str), list_schedules(), delete_schedule(&str), pause_schedule(&str), resume_schedule(&str), get_dlq_jobs(&str, u32)}`
- `RetentionConfig` is `crate::config::RetentionConfig` and implements `Default`.
- `RustQueueError::Internal(anyhow::Error)` exists.
- `MAX_ERROR_MESSAGE_LEN = 10_240` (`src/engine/queue.rs:34`).

---

## Task 1: Refactor `RustQueue` to `Arc` fields + `Clone` (no behavior change)

**Files:**
- Modify: `src/builder.rs:35-37` (struct), `src/builder.rs:119-133` (`build`), all delegating methods `src/builder.rs:138-218`
- Test: `tests/library_usage.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests/library_usage.rs`:

```rust
#[tokio::test]
async fn rustqueue_is_clone_and_shares_state() {
    use rustqueue::RustQueue;
    use serde_json::json;

    let rq = RustQueue::memory().build().unwrap();
    let rq2 = rq.clone();
    let id = rq.push("emails", "welcome", json!({"to": "a@b.com"}), None).await.unwrap();
    // The clone sees the job pushed via the original — shared storage.
    let job = rq2.get_job(id).await.unwrap();
    assert!(job.is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test library_usage rustqueue_is_clone_and_shares_state`
Expected: FAIL to compile — `RustQueue` does not implement `Clone`.

- [ ] **Step 3: Implement the refactor**

In `src/builder.rs`, change the struct (was `manager: QueueManager`):

```rust
use std::time::Duration;
use crate::housekeeping::HousekeepingState;

#[derive(Clone)]
pub struct RustQueue {
    manager: Arc<QueueManager>,
    housekeeping: Arc<HousekeepingState>,
    stall_timeout: Duration,
    tick_interval: Duration,
}
```

In `build()` (replace the final construction), default the new fields (builder knobs land in Task 2):

```rust
let manager = if let Some(registry) = self.worker_registry {
    QueueManager::new(storage).with_worker_registry(registry)
} else {
    QueueManager::new(storage)
};
Ok(RustQueue {
    manager: Arc::new(manager),
    housekeeping: Arc::new(HousekeepingState::new()),
    stall_timeout: Duration::from_secs(30),
    tick_interval: Duration::from_secs(1),
})
```

All delegating methods keep their bodies unchanged — `self.manager.push(...)` etc. still compile through the `Arc`.

> Note: Task 2 creates `HousekeepingState`. If implementing strictly in order, temporarily stub `HousekeepingState` as an empty struct with `fn new() -> Self`, then flesh it out in Task 2. Recommended: do Task 2's Step 3 (the `housekeeping.rs` file) first, then this task compiles cleanly.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS (all existing tests + the new clone test). The `Arc` change is transparent to callers.

- [ ] **Step 5: Commit**

```bash
git add src/builder.rs src/lib.rs src/housekeeping.rs tests/library_usage.rs
git commit -m "refactor: RustQueue holds Arc<QueueManager>, derive Clone"
```

---

## Task 2: `HousekeepingState` + builder knobs + validation

**Files:**
- Create: `src/housekeeping.rs`
- Modify: `src/lib.rs` (add `mod housekeeping;`), `src/builder.rs` (`RustQueueBuilder` fields + `stall_timeout`/`tick_interval` setters + `build` validation)
- Test: `tests/library_usage.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn build_rejects_zero_tick_interval() {
    use rustqueue::RustQueue;
    use std::time::Duration;

    let err = RustQueue::memory()
        .tick_interval(Duration::ZERO)
        .build()
        .err();
    assert!(err.is_some(), "zero tick_interval must be rejected");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test library_usage build_rejects_zero_tick_interval`
Expected: FAIL to compile — `tick_interval` method does not exist.

- [ ] **Step 3: Create `src/housekeeping.rs`**

```rust
//! Background housekeeping lifecycle for embedded `RustQueue`.
//!
//! Holds the scheduler `JoinHandle` and aborts it when the last `RustQueue`
//! clone is dropped. There is no public Drop-guard handle to forget.

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use tokio::task::JoinHandle;

pub(crate) struct HousekeepingState {
    /// Set to true once the scheduler loop has been spawned. Idempotency guard.
    pub(crate) started: AtomicBool,
    /// The spawned scheduler task. Aborted on Drop.
    pub(crate) task: Mutex<Option<JoinHandle<()>>>,
}

impl HousekeepingState {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            task: Mutex::new(None),
        }
    }
}

impl Drop for HousekeepingState {
    fn drop(&mut self) {
        if let Some(handle) = self.task.lock().unwrap().take() {
            handle.abort();
        }
    }
}
```

Add to `src/lib.rs` (with the other `pub mod`/`mod` lines):

```rust
mod housekeeping;
pub mod worker; // created in Task 5; add now or in Task 5
```

- [ ] **Step 4: Add builder knobs + validation in `src/builder.rs`**

Add fields to `RustQueueBuilder`:

```rust
pub struct RustQueueBuilder {
    storage: Arc<dyn StorageBackend>,
    buffered_config: Option<BufferedRedbConfig>,
    hybrid_config: Option<HybridConfig>,
    worker_registry: Option<Arc<WorkerRegistry>>,
    stall_timeout: Duration,
    tick_interval: Duration,
}
```

Initialise them in `memory()`, `redb()`, `hybrid()` (each constructs `RustQueueBuilder { ... }`):

```rust
stall_timeout: Duration::from_secs(30),
tick_interval: Duration::from_secs(1),
```

Add setters on `impl RustQueueBuilder`:

```rust
/// Set how long a job may be `Active` without a heartbeat before stall
/// detection reclaims it. Default 30s.
pub fn stall_timeout(mut self, d: Duration) -> Self {
    self.stall_timeout = d;
    self
}

/// Set the housekeeping tick interval (delayed-job promotion, schedules,
/// stall detection). Default 1s. Must be non-zero.
pub fn tick_interval(mut self, d: Duration) -> Self {
    self.tick_interval = d;
    self
}
```

In `build()`, validate before constructing, and carry the values into `RustQueue`:

```rust
if self.tick_interval.is_zero() {
    return Err(anyhow::anyhow!("tick_interval must be greater than zero"));
}
// ...existing storage + manager construction...
Ok(RustQueue {
    manager: Arc::new(manager),
    housekeeping: Arc::new(HousekeepingState::new()),
    stall_timeout: self.stall_timeout,
    tick_interval: self.tick_interval,
})
```

(`build()` returns `anyhow::Result<RustQueue>`, so `anyhow::anyhow!` is fine.)

- [ ] **Step 5: Run tests**

Run: `cargo test --test library_usage`
Expected: PASS, including `build_rejects_zero_tick_interval`.

- [ ] **Step 6: Commit**

```bash
git add src/housekeeping.rs src/lib.rs src/builder.rs tests/library_usage.rs
git commit -m "feat: HousekeepingState + stall_timeout/tick_interval builder knobs"
```

---

## Task 3: `start_housekeeping()` — idempotent, runtime-checked, lifetime-bound

**Files:**
- Modify: `src/builder.rs` (`impl RustQueue`)
- Test: `tests/library_usage.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn embedded_backoff_retry_is_promoted_by_housekeeping() {
    use rustqueue::RustQueue;
    use serde_json::json;
    use std::time::Duration;

    // Fast tick so the test is quick.
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    rq.start_housekeeping().unwrap();

    let id = rq.push("q", "job", json!({}), None).await.unwrap();
    let jobs = rq.pull("q", 1).await.unwrap();
    assert_eq!(jobs.len(), 1);
    // Fail it — default backoff routes to Delayed (~1000ms).
    rq.fail(id, "boom").await.unwrap();

    // Without housekeeping this would stall forever. Poll until it returns to Waiting.
    let mut promoted = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(j) = rq.get_job(id).await.unwrap() {
            use rustqueue::JobState;
            if matches!(j.state, JobState::Waiting) {
                promoted = true;
                break;
            }
        }
    }
    assert!(promoted, "delayed retry was never promoted back to Waiting");
}

#[test]
fn start_housekeeping_outside_runtime_errors() {
    use rustqueue::RustQueue;
    let rq = RustQueue::memory().build().unwrap();
    // No #[tokio::main] / runtime here on purpose.
    assert!(rq.start_housekeeping().is_err());
}

#[tokio::test]
async fn start_housekeeping_is_idempotent() {
    use rustqueue::RustQueue;
    let rq = RustQueue::memory().build().unwrap();
    rq.start_housekeeping().unwrap();
    // Second call (and a clone's call) must not spawn a second loop or error.
    rq.start_housekeeping().unwrap();
    rq.clone().start_housekeeping().unwrap();
}
```

(`JobState` must be re-exported from `lib.rs` — it already is.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test library_usage start_housekeeping`
Expected: FAIL to compile — `start_housekeeping` does not exist.

- [ ] **Step 3: Implement `start_housekeeping`**

Add to `impl RustQueue` in `src/builder.rs`:

```rust
use std::sync::atomic::Ordering;
use crate::config::RetentionConfig;

impl RustQueue {
    /// Start the background housekeeping loop: delayed-job promotion, schedule
    /// execution, timeout + stall detection, retention. Required for retries,
    /// crash recovery, and schedules to work in embedded mode.
    ///
    /// Idempotent — repeated calls (including from clones or `run_worker`) spawn
    /// only one loop. Returns `Err` if called outside a Tokio runtime. The loop
    /// is aborted automatically when the last `RustQueue` clone is dropped.
    pub fn start_housekeeping(&self) -> Result<(), RustQueueError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(RustQueueError::Internal(anyhow::anyhow!(
                "start_housekeeping must be called within a Tokio runtime"
            )));
        }
        // Win the race to spawn; losers return Ok without spawning.
        if self
            .housekeeping
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        let handle = crate::engine::scheduler::start_scheduler(
            self.manager.clone(),
            self.tick_interval.as_millis() as u64,
            self.stall_timeout.as_millis() as u64,
            RetentionConfig::default(),
        );
        *self.housekeeping.task.lock().unwrap() = Some(handle);
        Ok(())
    }
}
```

Confirm `start_scheduler` is reachable: it is `pub fn` in `src/engine/scheduler.rs`. If `engine` does not re-export it, reference it as `crate::engine::scheduler::start_scheduler` (module is `pub mod engine` → ensure `pub mod scheduler;` inside `src/engine/mod.rs`; add `pub` if missing).

- [ ] **Step 4: Run tests**

Run: `cargo test --test library_usage start_housekeeping && cargo test --test library_usage embedded_backoff_retry_is_promoted_by_housekeeping`
Expected: PASS — retry promoted, outside-runtime errors, idempotent.

- [ ] **Step 5: Commit**

```bash
git add src/builder.rs src/engine/mod.rs tests/library_usage.rs
git commit -m "feat: start_housekeeping (idempotent, runtime-checked, lifetime-bound)"
```

---

## Task 4: Expose schedule CRUD + heartbeat + DLQ on `RustQueue`

**Files:**
- Modify: `src/builder.rs` (`impl RustQueue` delegations)
- Test: `tests/library_usage.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn embedded_schedule_fires_via_housekeeping() {
    use rustqueue::RustQueue;
    use std::time::Duration;

    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    rq.start_housekeeping().unwrap();

    // Construct an interval Schedule the same way tests/schedule_integration.rs does.
    // It fires job pushes onto its target queue every `every_ms`.
    let schedule = make_interval_schedule("tick-sched", "scheduled_q", 100); // helper below
    rq.create_schedule(&schedule).await.unwrap();

    let listed = rq.list_schedules().await.unwrap();
    assert!(listed.iter().any(|s| s.name == "tick-sched"));

    // Wait for the scheduler to fire at least one job onto the target queue.
    let mut fired = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let counts = rq.get_queue_stats("scheduled_q").await.unwrap();
        if counts.waiting + counts.active + counts.completed > 0 {
            fired = true;
            break;
        }
    }
    assert!(fired, "schedule never fired a job in embedded mode");
}
```

> Implementation note: copy the `Schedule` construction from `tests/schedule_integration.rs` into a local `make_interval_schedule(name, queue, every_ms)` helper in this test file. Do not invent `Schedule` fields — mirror the existing test exactly. `QueueCounts` field names (`waiting`, `active`, `completed`) come from `src/engine/models.rs`; adjust if they differ.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test library_usage embedded_schedule_fires_via_housekeeping`
Expected: FAIL to compile — `create_schedule`/`list_schedules` not on `RustQueue`.

- [ ] **Step 3: Add delegations**

Add to `impl RustQueue` in `src/builder.rs` (mirror the existing delegation style):

```rust
use crate::engine::models::Schedule;

/// Create or update a schedule (cron or interval). Requires housekeeping
/// running for the schedule to fire.
pub async fn create_schedule(&self, schedule: &Schedule) -> Result<(), RustQueueError> {
    self.manager.create_schedule(schedule).await
}

/// List all schedules (active and paused).
pub async fn list_schedules(&self) -> Result<Vec<Schedule>, RustQueueError> {
    self.manager.list_schedules().await
}

/// Get a schedule by name.
pub async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>, RustQueueError> {
    self.manager.get_schedule(name).await
}

/// Delete a schedule by name.
pub async fn delete_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    self.manager.delete_schedule(name).await
}

/// Pause a schedule.
pub async fn pause_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    self.manager.pause_schedule(name).await
}

/// Resume a paused schedule.
pub async fn resume_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    self.manager.resume_schedule(name).await
}

/// Update the heartbeat for an active job (workers call this for long jobs).
pub async fn heartbeat(&self, id: JobId) -> Result<(), RustQueueError> {
    self.manager.heartbeat(id).await
}

/// List dead-letter-queue jobs for a queue, up to `limit`.
pub async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>, RustQueueError> {
    self.manager.get_dlq_jobs(queue, limit).await
}
```

Ensure `Schedule` is re-exported from `lib.rs` (it already is, per `src/lib.rs:38`).

- [ ] **Step 4: Run tests**

Run: `cargo test --test library_usage embedded_schedule_fires_via_housekeeping`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/builder.rs tests/library_usage.rs
git commit -m "feat: expose schedule CRUD, heartbeat, get_dlq_jobs on RustQueue"
```

---

## Task 5: `run_worker_with_shutdown` core loop (no heartbeat yet)

**Files:**
- Create: `src/worker.rs`
- Modify: `src/lib.rs` (`pub mod worker;` if not added in Task 2)
- Test: `tests/worker_runtime.rs` (new)

- [ ] **Step 1: Write the failing tests**

Create `tests/worker_runtime.rs`:

```rust
use rustqueue::RustQueue;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn run_worker_acks_on_ok_and_stops_on_shutdown() {
    let rq = RustQueue::memory().tick_interval(Duration::from_millis(50)).build().unwrap();
    let id = rq.push("emails", "send", json!({"to":"a@b.com"}), None).await.unwrap();

    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = seen.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown("emails", move |_job| {
                let seen2 = seen2.clone();
                async move { seen2.fetch_add(1, Ordering::SeqCst); Ok::<(), String>(()) }
            }, async move { let _ = rx.await; }).await
        })
    };

    // Give it time to process, then signal shutdown.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();

    assert_eq!(seen.load(Ordering::SeqCst), 1);
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(matches!(job.state, JobState::Completed));
}

#[tokio::test]
async fn run_worker_fails_on_err() {
    let rq = RustQueue::memory().tick_interval(Duration::from_millis(50)).build().unwrap();
    let id = rq.push("q", "job", json!({}), None).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown("q", |_job| async { Err::<(), String>("nope".into()) },
                async move { let _ = rx.await; }).await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();

    // After fail, default backoff routes to Delayed (not Active / not Completed).
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(matches!(job.state, JobState::Delayed | JobState::Waiting), "got {:?}", job.state);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test worker_runtime`
Expected: FAIL to compile — `run_worker_with_shutdown` does not exist.

- [ ] **Step 3: Implement `src/worker.rs`**

```rust
//! Managed worker loop for embedded `RustQueue` usage.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::builder::RustQueue;
use crate::engine::error::RustQueueError;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Keep worker error strings safely under the storage cap (MAX_ERROR_MESSAGE_LEN = 10_240).
const MAX_WORKER_ERR: usize = 8192;

fn truncate_err(mut s: String) -> String {
    if s.len() <= MAX_WORKER_ERR {
        return s;
    }
    let mut end = MAX_WORKER_ERR;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("…(truncated)");
    s
}

impl RustQueue {
    /// Run a managed worker on `queue` until Ctrl-C: pulls jobs, runs `handler`,
    /// acks on `Ok`, fails (engine retries/DLQ) on `Err`. Ensures housekeeping is
    /// running. Sequential (one job at a time) — this is minimum-viable
    /// correctness, not a high-throughput runtime (concurrency lands later).
    pub async fn run_worker<F, Fut, E>(&self, queue: &str, handler: F) -> Result<(), RustQueueError>
    where
        F: Fn(crate::Job) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: std::fmt::Display,
    {
        self.run_worker_with_shutdown(queue, handler, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Like [`run_worker`](Self::run_worker) but stops when `shutdown` resolves
    /// instead of on Ctrl-C. Use this when embedding in a web server: pass the
    /// server's graceful-shutdown signal. The in-flight job is finished before
    /// returning.
    pub async fn run_worker_with_shutdown<F, Fut, E, S>(
        &self,
        queue: &str,
        handler: F,
        shutdown: S,
    ) -> Result<(), RustQueueError>
    where
        F: Fn(crate::Job) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: std::fmt::Display,
        S: Future<Output = ()> + Send + 'static,
    {
        self.start_housekeeping()?;

        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = stop.clone();
            tokio::spawn(async move {
                shutdown.await;
                stop.store(true, Ordering::SeqCst);
            });
        }

        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let jobs = self.pull(queue, 1).await?;
            if jobs.is_empty() {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            for job in jobs {
                let job_id = job.id; // capture BEFORE moving job into handler
                let outcome = handler(job).await; // NOT wrapped in shutdown — finish the job
                match outcome {
                    Ok(()) => {
                        if let Err(e) = self.ack(job_id, None).await {
                            warn!(job_id = %job_id, error = %e, "worker ack failed");
                        }
                    }
                    Err(err) => {
                        let msg = truncate_err(err.to_string());
                        if let Err(e) = self.fail(job_id, &msg).await {
                            warn!(job_id = %job_id, error = %e, "worker fail failed");
                        }
                    }
                }
                if stop.load(Ordering::SeqCst) {
                    break; // finished the in-flight job, now stop
                }
            }
        }
        Ok(())
    }
}
```

Add to `src/lib.rs` if not already: `pub mod worker;`. Confirm `crate::Job` and `RustQueue` are accessible (Job re-exported at `lib.rs:37`; `RustQueue` is `pub use builder::RustQueue` — in `worker.rs` import via `crate::builder::RustQueue`).

- [ ] **Step 4: Run tests**

Run: `cargo test --test worker_runtime`
Expected: PASS — acks on Ok, fails on Err, stops after in-flight job on shutdown.

- [ ] **Step 5: Commit**

```bash
git add src/worker.rs src/lib.rs tests/worker_runtime.rs
git commit -m "feat: run_worker / run_worker_with_shutdown core loop"
```

---

## Task 6: Auto-heartbeat in the worker loop

**Files:**
- Modify: `src/worker.rs`
- Test: `tests/worker_runtime.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn long_handler_is_not_reclaimed_as_stalled() {
    // stall_timeout 1s; handler runs ~3s. Without heartbeat, detect_stalls would
    // reclaim it and the worker's ack would fail / the job would re-run.
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(100))
        .stall_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let id = rq.push("slow", "job", json!({}), None).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let runs = Arc::new(AtomicUsize::new(0));
    let runs2 = runs.clone();
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown("slow", move |_job| {
                let runs2 = runs2.clone();
                async move {
                    runs2.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    Ok::<(), String>(())
                }
            }, async move { let _ = rx.await; }).await
        })
    };

    tokio::time::sleep(Duration::from_secs(4)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1, "job ran more than once → it was reclaimed mid-flight");
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(matches!(job.state, JobState::Completed));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test worker_runtime long_handler_is_not_reclaimed_as_stalled`
Expected: FAIL — without heartbeat, `runs > 1` or final state is not `Completed`.

- [ ] **Step 3: Add the heartbeat side-task**

In `run_worker_with_shutdown`, compute the interval once before the loop:

```rust
let hb_interval = std::cmp::max(self.stall_timeout / 3, Duration::from_secs(1));
```

Wrap the handler call with a heartbeat task:

```rust
let job_id = job.id;
let hb = {
    let rq = self.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(hb_interval);
        tick.tick().await; // consume the immediate tick
        loop {
            tick.tick().await;
            if rq.heartbeat(job_id).await.is_err() {
                break; // job no longer Active (acked/failed/stalled)
            }
        }
    })
};
let outcome = handler(job).await;
hb.abort();
// ...existing match on outcome...
```

- [ ] **Step 4: Run tests**

Run: `cargo test --test worker_runtime`
Expected: PASS — long job runs exactly once and completes; earlier tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/worker.rs tests/worker_runtime.rs
git commit -m "feat: auto-heartbeat in-flight job in run_worker"
```

---

## Task 7: tokio `signal` feature

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Check the current tokio features**

Run: `grep -n '^tokio' Cargo.toml` (via Read tool if grep is blocked).
If `signal` is not in the tokio `features = [...]` list, `run_worker`'s `ctrl_c()` won't compile.

- [ ] **Step 2: Add `signal`**

Add `"signal"` to tokio's feature list (or use `features = ["full"]` if already full — then no change). Example:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync", "signal", "net", "io-util"] }
```

(Keep the existing features; only add `signal`.)

- [ ] **Step 3: Verify**

Run: `cargo build --tests`
Expected: PASS — `tokio::signal::ctrl_c` resolves.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: enable tokio signal feature for run_worker Ctrl-C"
```

---

## Task 8: Drop aborts housekeeping (no leaked task)

**Files:**
- Test: `tests/library_usage.rs`

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
async fn dropping_all_clones_aborts_housekeeping() {
    use rustqueue::RustQueue;
    use std::time::Duration;

    let rq = RustQueue::memory().tick_interval(Duration::from_millis(50)).build().unwrap();
    rq.start_housekeeping().unwrap();
    let rq2 = rq.clone();
    drop(rq);
    // Loop still alive via rq2.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(rq2);
    // Last clone gone → HousekeepingState::drop aborts the task. No assertion
    // beyond "no panic / no hang"; this documents the lifetime contract.
    tokio::time::sleep(Duration::from_millis(100)).await;
}
```

- [ ] **Step 2: Run**

Run: `cargo test --test library_usage dropping_all_clones_aborts_housekeeping`
Expected: PASS (already implemented in Task 2/3; this locks the contract).

- [ ] **Step 3: Commit**

```bash
git add tests/library_usage.rs
git commit -m "test: housekeeping task aborts when last RustQueue clone drops"
```

---

## Task 9: Crash-recovery demo — `examples/crash_recovery.rs`

**Files:**
- Create: `examples/crash_recovery.rs`

- [ ] **Step 1: Write the example**

```rust
//! Crash-survival demo: a job survives `kill -9` and completes after restart.
//!
//! Terminal 1:  cargo run --example crash_recovery -- worker
//!   (starts a worker on a redb-backed queue with a short stall timeout)
//! Then:        cargo run --example crash_recovery -- push
//!   (enqueues a 10s job; watch the worker start it)
//! Then in T1:  Ctrl-C is graceful — to simulate a crash use:  kill -9 <pid>
//! Restart T1:  cargo run --example crash_recovery -- worker
//!   After the stall timeout the job is reclaimed and runs to completion.

use rustqueue::RustQueue;
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let rq = RustQueue::redb("/tmp/rustqueue-crash-demo.db")?
        .stall_timeout(Duration::from_secs(3))
        .tick_interval(Duration::from_millis(500))
        .build()?;

    match mode.as_str() {
        "push" => {
            let id = rq.push("work", "slow-task", json!({"n": 1}), None).await?;
            println!("pushed job {id}");
        }
        "worker" => {
            println!("worker pid {} — process a slow job, then `kill -9` me mid-job", std::process::id());
            rq.run_worker("work", |job| async move {
                println!("[start] {} {}", job.id, job.data);
                for i in 1..=10 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    println!("[work ] {} step {i}/10", job.id);
                }
                println!("[done ] {}", job.id);
                Ok::<(), String>(())
            }).await?;
        }
        _ => eprintln!("usage: crash_recovery -- [push|worker]"),
    }
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles and runs**

Run: `cargo build --example crash_recovery`
Expected: PASS. (Manual run is the demo; not part of CI.)

- [ ] **Step 3: Commit**

```bash
git add examples/crash_recovery.rs
git commit -m "docs: add crash-recovery demo example"
```

---

## Task 10: Enhanced Axum example

**Files:**
- Modify: `examples/axum_background_jobs.rs`

- [ ] **Step 1: Update the example**

Make it production-shaped: clone `RustQueue` into a `tokio::spawn`ed worker using `run_worker_with_shutdown`, wired to the same shutdown signal Axum's `with_graceful_shutdown` uses; add a `POST /enqueue` route; keep the dashboard reachable. Use a realistic handler (simulated email send with latency and an occasional `Err` to exercise retry). Reference the existing example's setup for the `RqState`/router wiring (`src/axum_integration.rs`).

Shape:

```rust
let rq = RustQueue::redb("/tmp/rq-axum.db")?.build()?;
let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

let worker = {
    let rq = rq.clone();
    tokio::spawn(async move {
        let _ = rq.run_worker_with_shutdown("emails", |job| async move {
            // simulate work
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            tracing::info!("sent email for {}", job.id);
            Ok::<(), String>(())
        }, async move { let _ = shutdown_rx.await; }).await;
    })
};

// build router with a POST /enqueue handler that calls rq.push(...)
// serve with .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.ok(); })
// on shutdown: let _ = shutdown_tx.send(());  worker.await.ok();
```

- [ ] **Step 2: Verify**

Run: `cargo build --example axum_background_jobs --features axum-integration`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add examples/axum_background_jobs.rs
git commit -m "docs: production-shaped Axum example with managed worker + graceful shutdown"
```

---

## Task 11: Operational docs — `docs/production.md`

**Files:**
- Create: `docs/production.md`

- [ ] **Step 1: Write the guide**

Sections (prose, with the §4 spec content): crash-only design + `kill -9` safety; durability-modes table (redb fsync ~314/s safest, buffered batched, hybrid up-to-`snapshot_interval` loss — when to use which); retries + backoff + DLQ; crash recovery via stall detection (requires housekeeping); housekeeping (what it does, `run_worker` auto-ensures it or call `start_housekeeping`, tuning `stall_timeout`/`tick_interval`); heartbeat behavior + the long-job/`stall_timeout` relationship (CPU-bound handlers can't heartbeat); graceful shutdown via `run_worker_with_shutdown`. Include the crash-recovery walkthrough (commands + expected output from Task 9).

- [ ] **Step 2: Commit**

```bash
git add docs/production.md
git commit -m "docs: operational guide for embedded production use"
```

---

## Task 12: README production section

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a "Production" section**

Add the `run_worker` snippet, the crash-survival pitch, and a link to `docs/production.md`. Keep positioning consistent ("background jobs without infrastructure"; never "high-performance/distributed").

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: README production section"
```

---

## Task 13: Website — Pages HTML + dashboard static

**Files:**
- Modify/Create: `docs/*.html` (GitHub Pages — self-contained, inline CSS), `dashboard/static/*`

- [ ] **Step 1: Add a "Production guide" page**

Create a self-contained HTML page under `docs/` (inline ALL CSS — no `/dashboard/landing.css` references, per the Pages constraint) covering the production-guide highlights, and link it from `docs/index.html` and `docs/examples.html`. Mirror into `dashboard/static/` so the embedded server serves it too. Keep the existing nav consistent across pages.

- [ ] **Step 2: Verify dashboard tests still pass**

Run: `cargo test --test dashboard_tests`
Expected: PASS (assertions still find "Background Jobs" + "Without Infrastructure").

- [ ] **Step 3: Commit**

```bash
git add docs/ dashboard/static/
git commit -m "site: production guide page (Pages + embedded dashboard)"
```

---

## Task 14: Version bump to 0.3.0 + CHANGELOG

**Files:**
- Modify: `Cargo.toml`, `sdk/node/package.json`, `sdk/python/pyproject.toml`, `sdk/python/rustqueue/__init__.py`, `src/api/openapi.rs`, `CHANGELOG.md`

- [ ] **Step 1: Bump all five version locations to `0.3.0`**

Update each file's version string. (Cross-check the list in `CLAUDE.md` → Release Process.)

- [ ] **Step 2: Write the CHANGELOG entry**

Frame as a **bug fix + feature**:
- Fixed: embedded mode now runs housekeeping — backoff retries and crash recovery work without the server (previously stalled).
- Added: `run_worker` / `run_worker_with_shutdown` managed worker, `start_housekeeping`, embedded schedule API, `heartbeat`, `get_dlq_jobs`, `stall_timeout`/`tick_interval` builder knobs, `RustQueue: Clone`.
- Not a breaking change.

- [ ] **Step 3: Full verification triad**

Run: `cargo test --features sqlite && cargo clippy --all-targets --features sqlite,postgres,otel -- -D warnings && cargo fmt --check && cargo audit --ignore RUSTSEC-2023-0071`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock sdk/ src/api/openapi.rs CHANGELOG.md
git commit -m "chore: bump version to 0.3.0"
```

---

## Task 15: PR, then tag/release

**Files:** none (git/CI)

- [ ] **Step 1: Push and open the PR**

```bash
git push -u origin feat/v0.3.0-production-embedded
gh pr create --base main --title "v0.3.0 — production embedded release" --body "<summary of the fix + run_worker + docs>"
```

- [ ] **Step 2: After CI is green, merge**

```bash
gh pr checks <PR#> --watch
gh pr merge <PR#> --squash --delete-branch
```

- [ ] **Step 3: Tag (manual push so CI publish triggers)**

```bash
git checkout main && git pull
git tag -a v0.3.0 -m "v0.3.0 — production embedded release"
git push origin v0.3.0
```

(Per CLAUDE.md: GITHUB_TOKEN-created tags can't trigger workflows — push the tag manually as above.)

- [ ] **Step 4: Verify**

Confirm the publish workflow ran and crates.io shows 0.3.0; confirm GitHub Pages redeployed.

---

## Self-review notes

- **Spec coverage:** §1 housekeeping → Tasks 1-3, 8; §1 API surface → Task 4; §2 run_worker → Tasks 5-7; §3 Axum → Task 10; §4 docs → Task 11; §5 demo → Task 9; §6 README/website → Tasks 12-13; §7 release → Tasks 14-15. All spec test cases map to Tasks 3-8.
- **Type consistency:** `run_worker_with_shutdown` signature is identical in Tasks 5 and 6; `start_housekeeping` returns `Result<(), RustQueueError>` everywhere; `HousekeepingState` fields (`started`, `task`) match between Task 2's definition and Task 3's use.
- **Known soft spots flagged inline:** `Schedule`/`QueueCounts` field names must be mirrored from existing tests (Task 4); `mod engine::scheduler` may need `pub` (Task 3); tokio `signal` feature (Task 7).
