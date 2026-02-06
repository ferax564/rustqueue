# RustQueue Phase 4: Schedule Engine & Production Completeness — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Complete the schedule execution engine (cron + interval), schedule API, PostgreSQL serve-mode, and dashboard schedule panel — making RustQueue a fully operational job scheduler.

**Architecture:** The Schedule model and storage methods already exist across all 4 backends. This phase wires the scheduler tick loop to evaluate cron/interval expressions (via `croner` crate, already in Cargo.toml), adds HTTP CRUD endpoints for schedules, enables PostgreSQL in serve mode, and extends the dashboard with a schedules panel.

**Tech Stack:** Rust, axum, tokio, croner, serde, rust-embed (existing stack)

---

## Pre-Existing State

The following already exists and should NOT be recreated:

| Component | Location | What exists |
|-----------|----------|-------------|
| Schedule model | `src/engine/models.rs:162-185` | `Schedule` struct with cron_expr, every_ms, max_executions, paused, next_run_at, etc. |
| Storage trait methods | `src/storage/mod.rs:53-56` | `upsert_schedule`, `get_active_schedules`, `delete_schedule` |
| All 4 backend implementations | `src/storage/{redb,memory,sqlite,postgres}.rs` | Schedule CRUD in all backends |
| Scheduler tick loop | `src/engine/scheduler.rs` | Promote, timeout, stall, cleanup — but NO schedule execution |
| croner crate | `Cargo.toml:62` | `croner = "2"` — cron expression parser |
| QueueManager.push() | `src/engine/queue.rs:99-105` | `push(queue, name, data, opts)` |
| JobOptions | `src/engine/queue.rs:22-38` | Priority, delay, retries, timeout, etc. |
| Dashboard SPA | `dashboard/static/` | 4-panel dashboard (overview, queues, DLQ, events) |

---

## Detailed Tasks (10 tasks)

---

### Task 1: Add `get_schedule` and `list_all_schedules` to StorageBackend Trait

The trait currently has `get_active_schedules()` (returns only non-paused), but we need:
- `get_schedule(name)` — for single schedule retrieval (API `GET /schedules/{name}`)
- `list_all_schedules()` — for listing all schedules including paused (API `GET /schedules`)

**Files:**
- Modify: `src/storage/mod.rs` — add 2 new methods to `StorageBackend` trait
- Modify: `src/storage/memory.rs` — implement both methods
- Modify: `src/storage/redb.rs` — implement both methods
- Modify: `src/storage/sqlite.rs` — implement both methods (behind `#[cfg(feature = "sqlite")]`)
- Modify: `src/storage/postgres.rs` — implement both methods (behind `#[cfg(feature = "postgres")]`)
- Modify: `tests/storage_backend_tests.rs` — add 2 new schedule tests to `backend_tests!` macro

**Implementation:**

Add to trait in `src/storage/mod.rs` after `delete_schedule`:
```rust
async fn get_schedule(&self, name: &str) -> anyhow::Result<Option<Schedule>>;
async fn list_all_schedules(&self) -> anyhow::Result<Vec<Schedule>>;
```

For **MemoryStorage** (`src/storage/memory.rs`):
```rust
async fn get_schedule(&self, name: &str) -> anyhow::Result<Option<Schedule>> {
    let schedules = self.schedules.read().unwrap();
    Ok(schedules.get(name).cloned())
}

async fn list_all_schedules(&self) -> anyhow::Result<Vec<Schedule>> {
    let schedules = self.schedules.read().unwrap();
    Ok(schedules.values().cloned().collect())
}
```

For **RedbStorage**: look up by name bytes in SCHEDULES_TABLE, deserialize from JSON. `list_all_schedules` iterates entire table (no filter on `paused`).

For **SqliteStorage**: `SELECT data FROM schedules WHERE name = ?` and `SELECT data FROM schedules`.

For **PostgresStorage**: `SELECT data FROM schedules WHERE name = $1` and `SELECT data FROM schedules`.

**Tests** (add to `backend_tests!` macro):
```rust
// Test: get_schedule returns Some for existing, None for missing
// Test: list_all_schedules returns both paused and active schedules
```

**Commit:** `feat(storage): add get_schedule and list_all_schedules to StorageBackend trait`

---

### Task 2: Schedule CRUD Methods on QueueManager

Add schedule business logic methods to `QueueManager` so the API layer doesn't access storage directly.

**Files:**
- Modify: `src/engine/queue.rs` — add schedule methods section
- Modify: `src/engine/models.rs` — add `job_options` field to `Schedule`

**Implementation:**

First, add `job_options` to `Schedule` in `src/engine/models.rs`:
```rust
pub struct Schedule {
    // ... existing fields ...
    pub job_data: serde_json::Value,
    /// Options applied to jobs created by this schedule (priority, retries, timeout, etc.)
    #[serde(default)]
    pub job_options: Option<crate::engine::queue::JobOptions>,
    // ... rest of fields ...
}
```

Then add to `QueueManager` in `src/engine/queue.rs`:
```rust
// ── Schedule management ──────────────────────────────────────────────

pub async fn create_schedule(&self, schedule: &Schedule) -> Result<(), RustQueueError> {
    // Validate: must have cron_expr or every_ms (but not both)
    if schedule.cron_expr.is_none() && schedule.every_ms.is_none() {
        return Err(RustQueueError::ValidationError(
            "Schedule must have cron_expr or every_ms".into()
        ));
    }
    if schedule.cron_expr.is_some() && schedule.every_ms.is_some() {
        return Err(RustQueueError::ValidationError(
            "Schedule cannot have both cron_expr and every_ms".into()
        ));
    }
    // Validate cron expression if present
    if let Some(ref cron) = schedule.cron_expr {
        croner::Cron::new(cron).parse().map_err(|e| {
            RustQueueError::ValidationError(format!("Invalid cron expression: {e}"))
        })?;
    }
    self.storage.upsert_schedule(schedule).await
        .map_err(RustQueueError::Internal)
}

pub async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>, RustQueueError> {
    self.storage.get_schedule(name).await
        .map_err(RustQueueError::Internal)
}

pub async fn list_schedules(&self) -> Result<Vec<Schedule>, RustQueueError> {
    self.storage.list_all_schedules().await
        .map_err(RustQueueError::Internal)
}

pub async fn delete_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    self.storage.delete_schedule(name).await
        .map_err(RustQueueError::Internal)
}

pub async fn pause_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    let mut schedule = self.storage.get_schedule(name).await
        .map_err(RustQueueError::Internal)?
        .ok_or_else(|| RustQueueError::ScheduleNotFound(name.to_string()))?;
    schedule.paused = true;
    schedule.updated_at = Utc::now();
    self.storage.upsert_schedule(&schedule).await
        .map_err(RustQueueError::Internal)
}

pub async fn resume_schedule(&self, name: &str) -> Result<(), RustQueueError> {
    let mut schedule = self.storage.get_schedule(name).await
        .map_err(RustQueueError::Internal)?
        .ok_or_else(|| RustQueueError::ScheduleNotFound(name.to_string()))?;
    schedule.paused = false;
    schedule.updated_at = Utc::now();
    self.storage.upsert_schedule(&schedule).await
        .map_err(RustQueueError::Internal)
}
```

Also add `ScheduleNotFound` variant to `src/engine/error.rs`:
```rust
#[error("Schedule not found: {0}")]
ScheduleNotFound(String),
```
with `http_status() => 404` and `error_code() => "SCHEDULE_NOT_FOUND"`.

**Tests (3 unit tests in queue.rs):**
- `test_create_schedule_validates_cron` — invalid cron returns error
- `test_create_schedule_requires_timing` — no cron or every_ms returns error
- `test_pause_resume_schedule` — toggle paused state

**Commit:** `feat(engine): add schedule CRUD methods to QueueManager`

---

### Task 3: Schedule API Endpoints

Create the HTTP API surface for schedule management.

**Files:**
- Create: `src/api/schedules.rs`
- Modify: `src/api/mod.rs` — add `pub mod schedules;` and merge routes into protected group
- Test: `tests/schedule_api_tests.rs` — 5 integration tests

**Implementation:**

`src/api/schedules.rs`:
```rust
use std::sync::Arc;
use axum::{extract::{Path, State}, Json, Router};
use serde::{Deserialize, Serialize};
use crate::api::{ApiError, AppState};
use crate::engine::models::Schedule;
use crate::engine::queue::JobOptions;

// Request/Response types
#[derive(Deserialize)]
pub struct CreateScheduleRequest {
    pub name: String,
    pub queue: String,
    pub job_name: String,
    #[serde(default)]
    pub job_data: serde_json::Value,
    pub cron_expr: Option<String>,
    pub every_ms: Option<u64>,
    pub timezone: Option<String>,
    pub max_executions: Option<u64>,
    #[serde(default)]
    pub job_options: Option<JobOptions>,
}

#[derive(Serialize)]
pub struct ScheduleResponse { pub ok: bool, pub schedule: Schedule }

#[derive(Serialize)]
pub struct ScheduleListResponse { pub ok: bool, pub schedules: Vec<Schedule> }

#[derive(Serialize)]
pub struct OkResponse { pub ok: bool }

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/schedules", axum::routing::post(create_schedule).get(list_schedules))
        .route("/api/v1/schedules/{name}", axum::routing::get(get_schedule).delete(delete_schedule))
        .route("/api/v1/schedules/{name}/pause", axum::routing::post(pause_schedule))
        .route("/api/v1/schedules/{name}/resume", axum::routing::post(resume_schedule))
}
```

Handlers call `state.queue_manager.create_schedule(...)`, etc. Convert `CreateScheduleRequest` into `Schedule` struct (set `created_at`, `updated_at` to `Utc::now()`, `execution_count` to 0, `paused` to false). Compute initial `next_run_at` using `croner`.

**Tests (5):**
- `test_create_and_get_schedule` — POST, then GET, verify fields
- `test_list_schedules` — create 2, list, verify count
- `test_delete_schedule` — create, delete, GET returns 404
- `test_pause_and_resume` — create, pause, verify paused=true, resume, verify paused=false
- `test_create_invalid_cron_returns_400` — invalid expression returns validation error

**Commit:** `feat(api): add schedule CRUD endpoints with pause/resume`

---

### Task 4: Schedule Execution Engine

Wire cron/interval evaluation into the scheduler tick loop so schedules actually create jobs.

**Files:**
- Modify: `src/engine/queue.rs` — add `execute_schedules()` method
- Modify: `src/engine/scheduler.rs` — call `execute_schedules()` in tick loop
- Test: `tests/schedule_execution_tests.rs` — 4 integration tests

**Implementation:**

Add to `QueueManager` in `src/engine/queue.rs`:
```rust
/// Evaluate all active schedules and create jobs for those that are due.
///
/// For each active schedule:
/// 1. Check if `next_run_at <= now`
/// 2. If due, push a new job with the schedule's queue/name/data/options
/// 3. Increment `execution_count`
/// 4. Compute new `next_run_at` (cron or interval)
/// 5. If `max_executions` reached, pause the schedule
/// 6. Save updated schedule
pub async fn execute_schedules(&self) -> Result<u32, RustQueueError> {
    let schedules = self.storage.get_active_schedules().await
        .map_err(RustQueueError::Internal)?;
    let now = Utc::now();
    let mut fired = 0u32;

    for mut schedule in schedules {
        // Check if schedule is due
        let is_due = match schedule.next_run_at {
            Some(next) => next <= now,
            None => true, // First run — compute and fire
        };

        if !is_due { continue; }

        // Push job
        if let Err(e) = self.push(
            &schedule.queue,
            &schedule.job_name,
            schedule.job_data.clone(),
            schedule.job_options.clone(),
        ).await {
            tracing::warn!(schedule = %schedule.name, error = %e, "Schedule job push failed");
            continue;
        }

        fired += 1;
        schedule.execution_count += 1;
        schedule.last_run_at = Some(now);
        schedule.updated_at = now;

        // Compute next_run_at
        schedule.next_run_at = compute_next_run(&schedule, now);

        // Check max_executions
        if let Some(max) = schedule.max_executions {
            if schedule.execution_count >= max {
                schedule.paused = true;
            }
        }

        if let Err(e) = self.storage.upsert_schedule(&schedule).await {
            tracing::warn!(schedule = %schedule.name, error = %e, "Schedule update failed");
        }
    }

    if fired > 0 {
        tracing::info!(count = fired, "Schedules fired");
    }
    Ok(fired)
}
```

Add helper function in `queue.rs`:
```rust
fn compute_next_run(schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(ref cron_expr) = schedule.cron_expr {
        if let Ok(cron) = croner::Cron::new(cron_expr).parse() {
            return cron.find_next_occurrence(&after, false).ok()
                .map(|dt| dt.with_timezone(&Utc));
        }
    }
    if let Some(every_ms) = schedule.every_ms {
        return Some(after + Duration::milliseconds(every_ms as i64));
    }
    None
}
```

Add to scheduler tick loop in `src/engine/scheduler.rs` (after promote, before timeouts):
```rust
// Execute due schedules
if let Err(e) = manager.execute_schedules().await {
    warn!(error = %e, "Schedule execution failed");
}
```

**Tests (4):**
- `test_cron_schedule_creates_jobs` — create schedule with `*/1 * * * * *` (every second), wait 2s, verify jobs created
- `test_interval_schedule_creates_jobs` — create schedule with `every_ms: 200`, wait 500ms, verify at least 2 jobs
- `test_max_executions_pauses_schedule` — schedule with `max_executions: 2`, wait, verify paused after 2 firings
- `test_paused_schedule_does_not_fire` — create paused schedule, wait, verify no jobs

**Commit:** `feat(engine): add schedule execution engine with cron and interval support`

---

### Task 5: Dashboard Auth Hardening

Move dashboard routes behind auth when `auth.enabled = true`. Currently dashboard is always public.

**Files:**
- Modify: `src/api/mod.rs` — conditionally place dashboard in public or protected group
- Test: `tests/auth_tests.rs` — 2 new tests

**Implementation:**

In `src/api/mod.rs`, change the router to conditionally protect dashboard:
```rust
pub fn router(state: Arc<AppState>) -> axum::Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut public = axum::Router::new()
        .merge(health::routes())
        .merge(prometheus::routes());

    let mut protected = axum::Router::new()
        .merge(jobs::routes())
        .merge(queues::routes())
        .merge(websocket::routes())
        .merge(schedules::routes());

    // Dashboard is public when auth is disabled, protected when enabled
    if state.auth_config.enabled {
        protected = protected.merge(crate::dashboard::routes());
    } else {
        public = public.merge(crate::dashboard::routes());
    }

    let protected = protected.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::auth_middleware,
    ));

    public
        .merge(protected)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

**Tests (2):**
- `test_dashboard_returns_401_when_auth_enabled` — auth on, no token, `/dashboard` returns 401
- `test_dashboard_accessible_with_token_when_auth_enabled` — auth on, valid token, `/dashboard` returns 200

**Commit:** `feat(api): protect dashboard behind auth when authentication is enabled`

---

### Task 6: Enable PostgreSQL in Serve Mode

Currently `main.rs` line 206 has `bail!("PostgreSQL backend not yet implemented")`. Replace with actual initialization.

**Files:**
- Modify: `src/main.rs` — replace bail with PostgresStorage initialization
- Test: Manual (requires running Postgres instance)

**Implementation:**

Replace in `main.rs`:
```rust
#[cfg(feature = "postgres")]
rustqueue::config::StorageBackendType::Postgres => {
    let url = config.storage.postgres_url.as_ref()
        .ok_or_else(|| anyhow::anyhow!(
            "PostgreSQL backend requires storage.postgres_url in config or RUSTQUEUE_POSTGRES_URL env var"
        ))?;
    let s = Arc::new(rustqueue::storage::PostgresStorage::new(url).await?);
    info!(url = %url, "PostgresStorage initialized");
    s
}
```

Also check that `PostgresStorage::new()` exists and works. The `new()` method likely creates the pool and runs migrations.

**Commit:** `feat: enable PostgreSQL backend in serve mode`

---

### Task 7: Schedule TCP Commands

Add `schedule_create`, `schedule_list`, `schedule_delete`, `schedule_pause`, `schedule_resume` TCP commands.

**Files:**
- Modify: `src/protocol/handler.rs` — add schedule command handlers

**Implementation:**

Add to the `match cmd` in the TCP handler:
```rust
"schedule_create" => {
    // Parse schedule fields from JSON request
    let name = req["name"].as_str().ok_or("missing name")?;
    let queue = req["queue"].as_str().ok_or("missing queue")?;
    // ... construct Schedule, call manager.create_schedule(...)
}
"schedule_list" => {
    let schedules = manager.list_schedules().await?;
    // Return as JSON array
}
"schedule_get" => {
    let name = req["name"].as_str().ok_or("missing name")?;
    // Return single schedule
}
"schedule_delete" => {
    let name = req["name"].as_str().ok_or("missing name")?;
    manager.delete_schedule(name).await?;
}
"schedule_pause" => {
    let name = req["name"].as_str().ok_or("missing name")?;
    manager.pause_schedule(name).await?;
}
"schedule_resume" => {
    let name = req["name"].as_str().ok_or("missing name")?;
    manager.resume_schedule(name).await?;
}
```

**Tests (2 in `tests/tcp_protocol.rs`):**
- `test_tcp_schedule_create_and_list`
- `test_tcp_schedule_pause_resume`

**Commit:** `feat(protocol): add schedule management TCP commands`

---

### Task 8: Schedule CLI Commands

Add `rustqueue schedules list/create/delete/pause/resume` subcommands.

**Files:**
- Modify: `src/main.rs` — add `Schedules` subcommand with nested commands

**Implementation:**

```rust
#[cfg(feature = "cli")]
Schedules {
    #[command(subcommand)]
    action: ScheduleAction,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 6790)]
    http_port: u16,
},
```

```rust
#[cfg(feature = "cli")]
#[derive(clap::Subcommand)]
enum ScheduleAction {
    List,
    Create {
        #[arg(long)] name: String,
        #[arg(long)] queue: String,
        #[arg(long)] job_name: String,
        #[arg(long, default_value = "{}")] data: String,
        #[arg(long)] cron: Option<String>,
        #[arg(long)] every_ms: Option<u64>,
    },
    Delete { name: String },
    Pause { name: String },
    Resume { name: String },
}
```

Each action calls the corresponding HTTP endpoint using reqwest.

**Commit:** `feat(cli): add schedule management commands`

---

### Task 9: Dashboard Schedules Panel

Add a "Schedules" panel to the embedded web dashboard.

**Files:**
- Modify: `dashboard/static/index.html` — add sidebar nav item + panel section
- Modify: `dashboard/static/app.js` — add `renderSchedules()`, wire into navigation and refresh
- Modify: `dashboard/static/style.css` — add schedule-specific styles if needed

**Implementation:**

Add sidebar nav item (after DLQ):
```html
<li class="sidebar-nav-item">
    <a class="sidebar-nav-link" data-panel="schedules" href="#">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <circle cx="12" cy="12" r="10"/>
            <polyline points="12 6 12 12 16 14"/>
        </svg>
        Schedules
    </a>
</li>
```

Add panel section:
```html
<section class="panel" id="panel-schedules">
    <div class="panel-header">
        <div>
            <h1 class="panel-title">Schedules</h1>
            <p class="panel-subtitle">Cron and interval job schedules</p>
        </div>
    </div>
    <div class="schedules-grid" id="schedules-grid">
        <!-- Populated by app.js -->
    </div>
</section>
```

In `app.js`, add `renderSchedules()`:
- Fetch `GET /api/v1/schedules`
- Display each schedule as a card with: name, queue, cron/interval, last_run_at, next_run_at, execution_count, paused status
- Add pause/resume toggle button per schedule

**Commit:** `feat(dashboard): add schedules management panel`

---

### Task 10: Schedule Integration Tests + Final Wiring

End-to-end tests that verify the full schedule lifecycle: create via API, scheduler fires, jobs appear.

**Files:**
- Create: `tests/schedule_integration.rs` — comprehensive integration test
- Verify: all existing tests still pass

**Implementation:**

```rust
#[tokio::test]
async fn test_schedule_full_lifecycle() {
    // 1. Start test server with scheduler (short tick interval)
    // 2. Create interval schedule via POST /api/v1/schedules (every_ms: 200)
    // 3. Wait 600ms
    // 4. GET /api/v1/schedules/{name} — verify execution_count >= 2
    // 5. Pull jobs from target queue — verify jobs were created
    // 6. Pause schedule
    // 7. Record job count
    // 8. Wait 400ms
    // 9. Verify no new jobs created while paused
    // 10. Resume schedule
    // 11. Wait 400ms
    // 12. Verify new jobs appeared
    // 13. Delete schedule
    // 14. GET returns 404
}
```

Also test cron schedule with `* * * * * *` (every second).

Run full test suite: `cargo test` and `cargo test --features sqlite`.

**Commit:** `test: add comprehensive schedule lifecycle integration tests`

---

## Dependency Graph

```
Task 1 (Storage methods) ──────────────────────┐
    │                                           │
    ▼                                           │
Task 2 (QueueManager methods) ─────────────────┤
    │                                           │
    ├──► Task 3 (Schedule API)                  │
    │                                           │
    ├──► Task 4 (Execution Engine) ◄── croner   │
    │                                           │
    ├──► Task 7 (TCP Commands)                  │
    │                                           │
    └──► Task 8 (CLI Commands)                  │
                                                │
Task 5 (Dashboard Auth) ─── standalone          │
Task 6 (Postgres Serve) ─── standalone          │
Task 9 (Dashboard Panel) ◄── Task 3            │
                                                │
Task 10 (Integration Tests) ◄─────────────────┘
```

**Recommended execution order:**
1. Task 1 → 2. Task 2 → 3. Task 4 → 4. Task 3 → 5. Task 5 → 6. Task 6 → 7. Task 7 → 8. Task 8 → 9. Task 9 → 10. Task 10

---

## Phase 4 Exit Criteria

- [ ] `cargo test` — all 170+ tests pass
- [ ] `cargo test --features sqlite` — all tests pass including sqlite
- [ ] `cargo clippy --features sqlite,postgres,otel` — no warnings
- [ ] Schedule CRUD works via HTTP, TCP, and CLI
- [ ] Cron schedules fire at the correct times
- [ ] Interval schedules fire at the correct intervals
- [ ] `max_executions` stops firing when limit reached
- [ ] Pause/resume correctly halts and resumes firing
- [ ] Dashboard shows schedule list with status
- [ ] PostgreSQL backend works in serve mode
- [ ] Dashboard protected by auth when `auth.enabled = true`

---

## Critical Files Reference

| File | Role | Phase 4 Changes |
|------|------|-----------------|
| `src/storage/mod.rs` | StorageBackend trait (18→20 methods) | Add `get_schedule`, `list_all_schedules` |
| `src/storage/{memory,redb,sqlite,postgres}.rs` | Backend implementations | Implement 2 new methods each |
| `src/engine/queue.rs` | QueueManager | Add schedule CRUD + `execute_schedules()` + `compute_next_run()` |
| `src/engine/scheduler.rs` | Background tick loop | Add `execute_schedules()` call |
| `src/engine/models.rs` | Schedule struct | Add `job_options` field |
| `src/engine/error.rs` | Error types | Add `ScheduleNotFound` variant |
| `src/api/schedules.rs` | **NEW** — Schedule HTTP endpoints | Full CRUD + pause/resume |
| `src/api/mod.rs` | Router composition | Add schedules module, conditional dashboard auth |
| `src/protocol/handler.rs` | TCP handler | Add 6 schedule commands |
| `src/main.rs` | CLI + serve startup | Schedule CLI commands, enable Postgres |
| `dashboard/static/index.html` | Dashboard HTML | Add schedules panel |
| `dashboard/static/app.js` | Dashboard JS | Add `renderSchedules()` |
| `Cargo.toml` | Dependencies | No changes needed (croner already present) |
