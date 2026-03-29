# Blue Ocean Repositioning: "Background Jobs Without Infrastructure"

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reposition RustQueue from "faster RabbitMQ" (red ocean) to "the SQLite of job queues" (blue ocean) — an embedded-first library that makes infrastructure-grade background jobs available to every developer with zero operational overhead.

**Architecture:** Three workstreams executed in order: (1) Rewrite all public-facing copy — README, landing page, tagline, meta descriptions — to lead with the embedded library story and kill the benchmark-first positioning. (2) Create a Rust `examples/` directory with progressive-disclosure examples that serve as both documentation and the primary onboarding path. (3) Build an Axum integration layer (`rustqueue::axum`) that makes RustQueue a first-class Axum extractor, turning "add background jobs to your Axum app" into a 3-line change.

**Tech Stack:** Rust, Axum 0.8, HTML/CSS (landing page), Markdown (README)

---

## Research Findings (Reference)

### Market Context

- **SQLite's playbook:** "Think of SQLite not as a replacement for Oracle but as a replacement for `fopen()`." SQLite doesn't compete on the database value curve — it created a new category (embedded-first) and now has 1 trillion+ active databases.
- **DuckDB copied it:** Zero dependencies, single-file, pip-installable, no server. Millions of monthly downloads.
- **Rust job queue vacuum:** The entire Rust job queue ecosystem (rusty-sidekiq, fang, backie, background-jobs) totals ~600K all-time downloads combined. Every crate requires Redis or Postgres. There is no embedded-first option.
- **"Without Redis" movement is real:** Rails 8 shipped SolidQueue as default (no Redis). River (Go + Postgres) growing fast. bunqueue (Bun + SQLite) launched to viral reception. Active HN threads questioning Redis as a job queue dependency.
- **Tier 3 non-customers are massive:** Solo devs, small SaaS builders, side-project creators who process everything in the request cycle because "a real queue is overkill." No queue product targets them.

### Current Positioning Problems

1. README leads with "High-performance distributed job scheduler" — triggers enterprise evaluation criteria RustQueue can't satisfy at v0.1.0.
2. Landing page hero: "Faster than RabbitMQ" — places RustQueue on the red ocean benchmark curve.
3. Benchmark table is the visual centerpiece — invites direct comparison with 15+ year incumbents.
4. "5 storage backends" marketed as a feature — creates "which one?" decision paralysis.
5. Embedded library usage is buried below benchmarks and server instructions in README.
6. No Rust examples directory exists at all.
7. No framework integration — users must wire QueueManager into Axum state manually.

### The New Positioning

**Tagline:** "Background jobs without infrastructure."

**Positioning statement (SQLite-style):** "Think of RustQueue not as a replacement for RabbitMQ, but as a replacement for spawning a thread."

**Entry point:** `use rustqueue::RustQueue` — a library import, not a server deployment.

**Growth path:** Embedded (in-process) → Standalone server (same data file) → Scaled deployment (future).

---

## File Structure

### Modified Files

| File | Responsibility |
|------|---------------|
| `README.md` | Project landing — rewritten to lead with embedded library story |
| `dashboard/static/landing.html` | Web landing page — new hero, new narrative, embedded-first |
| `dashboard/static/landing.css` | Landing page styles — new hero section with code block styling |
| `src/lib.rs` | Library root — add `pub mod axum_integration` behind feature gate |
| `Cargo.toml` | Add `axum-integration` feature flag, no new deps |

### New Files

| File | Responsibility |
|------|---------------|
| `examples/basic.rs` | Simplest possible: push, pull, ack in 15 lines |
| `examples/axum_background_jobs.rs` | Axum web app with background email queue |
| `examples/persistent.rs` | File-backed queue that survives restarts |
| `examples/worker.rs` | Long-running worker loop pattern |
| `src/axum_integration.rs` | `RqState` extractor + `rq_layer()` helper for Axum apps |
| `tests/axum_integration_test.rs` | Integration tests for the Axum extractor |

---

## Task 1: Rewrite README — Embedded-First Narrative

**Files:**
- Modify: `README.md` (full rewrite)

- [ ] **Step 1: Read current README for reference**

Run: Review current `README.md` (already read — 119 lines, benchmark-table-first layout).

- [ ] **Step 2: Write the new README**

Replace the full content of `README.md` with:

```markdown
# RustQueue

**Background jobs without infrastructure.**

[![CI](https://github.com/ferax564/rustqueue/actions/workflows/ci.yml/badge.svg)](https://github.com/ferax564/rustqueue/actions/workflows/ci.yml)
[![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

Add background job processing to any Rust application. No Redis. No RabbitMQ. No external services. Just a library.

```rust
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("./jobs.db")?.build()?;

    // Push a job
    let id = rq.push("emails", "send-welcome", json!({"to": "user@example.com"}), None).await?;

    // Pull and process
    let jobs = rq.pull("emails", 1).await?;
    println!("Processing: {}", jobs[0].name);
    rq.ack(jobs[0].id, None).await?;

    Ok(())
}
```

That's it. Jobs persist to disk automatically. Retries, backoff, and dead-letter are built in. Your data survives restarts and crashes.

## When You Outgrow Embedded Mode

Run the same storage as a standalone server — no migration, no data export:

```bash
cargo install rustqueue
rustqueue serve
```

Now you have a REST API, a TCP protocol, a web dashboard, and client SDKs for Node.js, Python, and Go. Same engine, same guarantees.

## Why RustQueue

Think of RustQueue not as a replacement for RabbitMQ, but as a replacement for `tokio::spawn`.

Most applications don't need a message broker. They need a way to run tasks in the background that won't be lost if the process crashes. RustQueue gives you that as a library call — and grows into a full server when you're ready.

| | RustQueue | Redis + BullMQ | RabbitMQ |
|---|---|---|---|
| External dependencies | **None** | Redis server | Erlang + RabbitMQ server |
| Time to first job | **2 minutes** | 15–30 minutes | 15–30 minutes |
| Deployment | Library import or single binary | 2+ services | 2+ services |
| Binary size | **6.8 MB** | N/A | N/A |
| Startup time | **10ms** | Seconds | Seconds |
| Idle memory | **~15 MB** | ~100 MB+ | ~100 MB+ |

## Examples

```bash
cargo run --example basic              # Push, pull, ack — 15 lines
cargo run --example persistent         # File-backed queue surviving restarts
cargo run --example worker             # Long-running worker loop
cargo run --example axum_background_jobs  # Background jobs in an Axum web app
```

## Features

- **Persistent storage** — ACID-compliant embedded database (redb). Crash-safe.
- **Retries + backoff + DLQ** — Fixed, linear, or exponential. Dead-letter queue for inspection.
- **Cron and interval scheduling** — Full schedule engine with pause/resume.
- **DAG workflows** — Job dependencies with cycle detection and cascade failure.
- **Progress tracking** — 0–100 progress with log messages. Worker heartbeats.
- **Webhooks** — HMAC-SHA256 signed HTTP callbacks on job events.
- **Real-time events** — WebSocket stream of job lifecycle changes.
- **Web dashboard** — Built-in UI at `/dashboard` when running as a server.
- **Client SDKs** — Node.js, Python, Go — zero runtime dependencies each.
- **Observability** — 15+ Prometheus metrics, pre-built Grafana dashboard.

## Server Mode

When you need a standalone deployment:

```bash
# Binary
cargo install rustqueue && rustqueue serve

# Docker
docker compose up -d

# Docker with Prometheus + Grafana
docker compose -f docker-compose.monitoring.yml up -d
```

Full REST API, TCP protocol, and management CLI. See [API docs](docs/orchestration-api.md).

## Storage Backends

The default `redb` backend handles most workloads. For specialized needs:

| Backend | When to use |
|---------|------------|
| **redb** (default) | Production. ACID, crash-safe, zero config. |
| **Hybrid** | High throughput. Memory hot path + periodic disk snapshots. |
| **SQLite** | If you want SQL access to job data. `--features sqlite` |
| **PostgreSQL** | Multi-node shared storage. `--features postgres` |
| **In-memory** | Tests and ephemeral workloads. |

## Performance

For throughput-sensitive workloads, RustQueue's hybrid TCP mode delivers 40,000+ produce ops/sec and 26,000+ consume ops/sec — competitive with RabbitMQ on produce, 5x faster on consume. See [benchmark details](docs/competitor-benchmark-2026-03-28.md).

## Development

```bash
cargo test                           # Run all tests
cargo test --features sqlite         # Include SQLite backend
cargo clippy -- -D warnings          # Lint
cargo bench                          # Throughput benchmarks
```

## License

MIT OR Apache-2.0
```

- [ ] **Step 3: Review the diff**

Run: `git diff README.md`

Verify:
- First code block is the embedded Rust library usage (not curl/server)
- No benchmark table in the first screen
- "Background jobs without infrastructure" is the tagline
- Performance section exists but is below features, as a single paragraph with a link
- Storage backends section leads with "the default handles most workloads"
- The comparison table compares setup experience, not ops/sec

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: rewrite README — lead with embedded library, not benchmarks

Repositions RustQueue from 'faster RabbitMQ' to 'background jobs without
infrastructure'. Embedded Rust library usage is now the first thing visitors
see. Benchmark details moved to a reference link. Comparison table focuses
on setup experience (time to first job, dependencies, memory) rather than
raw throughput."
```

---

## Task 2: Create Basic Example — `examples/basic.rs`

**Files:**
- Create: `examples/basic.rs`

- [ ] **Step 1: Write the example**

```rust
//! Simplest RustQueue usage: push a job, pull it, acknowledge it.
//!
//! Run with: `cargo run --example basic`

use rustqueue::{RustQueue, JobOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // In-memory backend — great for trying things out.
    // Switch to RustQueue::redb("./jobs.db")? for persistence.
    let rq = RustQueue::memory().build()?;

    // Push a job onto the "emails" queue
    let id = rq.push(
        "emails",
        "send-welcome",
        json!({"to": "user@example.com", "template": "welcome"}),
        None,
    ).await?;
    println!("Pushed job: {id}");

    // Pull one job from the queue
    let jobs = rq.pull("emails", 1).await?;
    let job = &jobs[0];
    println!("Processing: {} (data: {})", job.name, job.data);

    // Acknowledge completion
    rq.ack(job.id, None).await?;
    println!("Done!");

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles and runs**

Run: `cargo run --example basic`

Expected output:
```
Pushed job: <uuid>
Processing: send-welcome (data: {"template":"welcome","to":"user@example.com"})
Done!
```

- [ ] **Step 3: Commit**

```bash
git add examples/basic.rs
git commit -m "examples: add basic push/pull/ack example"
```

---

## Task 3: Create Persistent Example — `examples/persistent.rs`

**Files:**
- Create: `examples/persistent.rs`

- [ ] **Step 1: Write the example**

```rust
//! Persistent queue that survives process restarts.
//!
//! Run this twice to see that jobs pushed in the first run
//! are still available in the second run.
//!
//! Run with: `cargo run --example persistent`

use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = "/tmp/rustqueue-example.db";
    let rq = RustQueue::redb(db_path)?.build()?;

    // Check if there are jobs from a previous run
    let existing = rq.pull("tasks", 1).await?;
    if !existing.is_empty() {
        let job = &existing[0];
        println!("Found job from previous run: {} (data: {})", job.name, job.data);
        rq.ack(job.id, None).await?;
        println!("Acknowledged. Run again to push a new one.");
        return Ok(());
    }

    // No existing jobs — push one and exit without processing
    let id = rq.push(
        "tasks",
        "generate-report",
        json!({"report": "monthly", "month": "march"}),
        None,
    ).await?;
    println!("Pushed job {id} to {db_path}");
    println!("Run this example again to see the job survive the restart.");

    Ok(())
}
```

- [ ] **Step 2: Verify it works across two runs**

Run: `cargo run --example persistent`

First run expected:
```
Pushed job <uuid> to /tmp/rustqueue-example.db
Run this example again to see the job survive the restart.
```

Run: `cargo run --example persistent`

Second run expected:
```
Found job from previous run: generate-report (data: {"month":"march","report":"monthly"})
Acknowledged. Run again to push a new one.
```

- [ ] **Step 3: Commit**

```bash
git add examples/persistent.rs
git commit -m "examples: add persistent queue example — survives restarts"
```

---

## Task 4: Create Worker Loop Example — `examples/worker.rs`

**Files:**
- Create: `examples/worker.rs`

- [ ] **Step 1: Write the example**

```rust
//! Long-running worker that processes jobs as they arrive.
//!
//! Run with: `cargo run --example worker`
//!
//! In a second terminal, push jobs:
//!   cargo run --example basic

use rustqueue::RustQueue;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("/tmp/rustqueue-worker.db")?.build()?;

    println!("Worker started. Waiting for jobs on 'emails' queue...");
    println!("Push jobs with: cargo run --example basic\n");

    loop {
        let jobs = rq.pull("emails", 5).await?;

        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        for job in &jobs {
            println!("[{}] Processing: {} — {}", job.queue, job.name, job.data);

            // Simulate work
            tokio::time::sleep(Duration::from_millis(100)).await;

            rq.ack(job.id, None).await?;
            println!("[{}] Done: {}", job.queue, job.id);
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --example worker`

Expected: compiles without errors.

- [ ] **Step 3: Commit**

```bash
git add examples/worker.rs
git commit -m "examples: add long-running worker loop example"
```

---

## Task 5: Build Axum Integration — `src/axum_integration.rs`

This is the key framework integration. It lets any Axum app add background jobs with 3 lines of code.

**Files:**
- Create: `src/axum_integration.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write the failing test**

Create `tests/axum_integration_test.rs`:

```rust
//! Integration tests for the Axum extractor.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::axum_integration::RqState;
use rustqueue::RustQueue;
use serde_json::json;
use std::sync::Arc;

async fn push_handler(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let name = body["name"].as_str().unwrap_or("task");
    let id = rq.push("default", name, body, None).await.unwrap();
    Json(json!({"id": id.to_string()}))
}

async fn stats_handler(rq: RqState) -> Json<serde_json::Value> {
    let queues = rq.list_queues().await.unwrap();
    Json(json!({"queues": queues.len()}))
}

fn app(rq: RustQueue) -> Router {
    Router::new()
        .route("/push", post(push_handler))
        .route("/stats", get(stats_handler))
        .with_state(Arc::new(rq))
}

#[tokio::test]
async fn test_rqstate_extractor_push_and_stats() {
    let rq = RustQueue::memory().build().unwrap();
    let app = app(rq);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    // Push a job
    let resp = client
        .post(format!("http://{addr}/push"))
        .json(&json!({"name": "test-job", "data": 42}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["id"].as_str().is_some());

    // Check stats
    let resp = client
        .get(format!("http://{addr}/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["queues"].as_u64().unwrap(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test axum_integration_test`

Expected: compile error — `rustqueue::axum_integration` module does not exist.

- [ ] **Step 3: Add the feature flag to Cargo.toml**

In `Cargo.toml`, add to the `[features]` section:

```toml
axum-integration = []
```

No new dependencies needed — `axum` is already a dependency.

- [ ] **Step 4: Write the axum_integration module**

Create `src/axum_integration.rs`:

```rust
//! Axum integration for RustQueue.
//!
//! Provides [`RqState`], an Axum extractor that gives handlers direct access
//! to the [`RustQueue`] instance through `Arc<RustQueue>` application state.
//!
//! # Quick start
//!
//! ```no_run
//! use axum::{Router, Json, routing::post};
//! use rustqueue::RustQueue;
//! use rustqueue::axum_integration::RqState;
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! async fn enqueue(rq: RqState) -> Json<serde_json::Value> {
//!     let id = rq.push("emails", "send-welcome", json!({}), None).await.unwrap();
//!     Json(json!({"id": id.to_string()}))
//! }
//!
//! # async fn example() {
//! let rq = RustQueue::memory().build().unwrap();
//! let app = Router::new()
//!     .route("/enqueue", post(enqueue))
//!     .with_state(Arc::new(rq));
//! # }
//! ```

use std::ops::Deref;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use http::request::Parts;

use crate::builder::RustQueue;

/// Axum extractor that provides access to the [`RustQueue`] instance.
///
/// Expects `Arc<RustQueue>` as the router state. Use it as a handler
/// parameter and call any `RustQueue` method directly:
///
/// ```ignore
/// async fn handler(rq: RqState) -> impl IntoResponse {
///     rq.push("queue", "job", json!({}), None).await.unwrap();
/// }
/// ```
pub struct RqState(pub Arc<RustQueue>);

impl Deref for RqState {
    type Target = RustQueue;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<Arc<RustQueue>> for RqState {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        _parts: &mut Parts,
        state: &Arc<RustQueue>,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(RqState(Arc::clone(state))))
    }
}
```

- [ ] **Step 5: Wire up the module in lib.rs**

Add to `src/lib.rs` after the existing `pub mod storage;` line:

```rust
pub mod axum_integration;
```

- [ ] **Step 6: Run the test**

Run: `cargo test --test axum_integration_test`

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/axum_integration.rs src/lib.rs Cargo.toml tests/axum_integration_test.rs
git commit -m "feat: add Axum integration — RqState extractor for background jobs

Introduces rustqueue::axum_integration::RqState, an Axum FromRequestParts
extractor that provides RustQueue to handlers via Arc<RustQueue> state.
Three lines to add background jobs to any Axum app."
```

---

## Task 6: Create Axum Example — `examples/axum_background_jobs.rs`

**Files:**
- Create: `examples/axum_background_jobs.rs`

- [ ] **Step 1: Write the example**

```rust
//! Background jobs in an Axum web application.
//!
//! Demonstrates adding a persistent job queue to an Axum app with
//! zero external dependencies — no Redis, no RabbitMQ, no Docker.
//!
//! Run with: `cargo run --example axum_background_jobs`
//! Then:     `curl -X POST http://localhost:3000/send-email -H 'Content-Type: application/json' -d '{"to":"user@example.com"}'`

use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::axum_integration::RqState;
use rustqueue::RustQueue;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// POST /send-email — enqueues a welcome email job
async fn send_email(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let to = body["to"].as_str().unwrap_or("unknown");
    let id = rq
        .push("emails", "send-welcome", json!({"to": to}), None)
        .await
        .unwrap();
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

/// GET /stats — shows queue depth
async fn stats(rq: RqState) -> Json<serde_json::Value> {
    let queues = rq.list_queues().await.unwrap();
    Json(json!({"queues": queues}))
}

/// Background worker that processes email jobs
async fn email_worker(rq: Arc<RustQueue>) {
    println!("[worker] Listening for email jobs...");
    loop {
        let jobs = rq.pull("emails", 10).await.unwrap();
        for job in &jobs {
            let to = job.data["to"].as_str().unwrap_or("?");
            println!("[worker] Sending email to {to} (job {})", job.id);
            // Simulate sending an email
            tokio::time::sleep(Duration::from_millis(50)).await;
            rq.ack(job.id, None).await.unwrap();
        }
        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("/tmp/rustqueue-axum-example.db")?.build()?;
    let rq = Arc::new(rq);

    // Spawn the background worker
    tokio::spawn(email_worker(Arc::clone(&rq)));

    // Build the Axum app
    let app = Router::new()
        .route("/send-email", post(send_email))
        .route("/stats", get(stats))
        .with_state(rq);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Server running on http://localhost:3000");
    println!("Try: curl -X POST http://localhost:3000/send-email -H 'Content-Type: application/json' -d '{{\"to\":\"user@example.com\"}}'");
    axum::serve(listener, app).await?;

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --example axum_background_jobs`

Expected: compiles without errors.

- [ ] **Step 3: Commit**

```bash
git add examples/axum_background_jobs.rs
git commit -m "examples: add Axum background jobs — zero-infra web app demo"
```

---

## Task 7: Rewrite Landing Page — Embedded-First Hero

**Files:**
- Modify: `dashboard/static/landing.html`
- Modify: `dashboard/static/landing.css`

- [ ] **Step 1: Rewrite the landing page HTML**

Replace the full content of `dashboard/static/landing.html` with:

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="description" content="RustQueue: background jobs without infrastructure. Add persistent job processing to any Rust app with zero external dependencies.">
    <title>RustQueue | Background Jobs Without Infrastructure</title>
    <link rel="preconnect" href="https://fonts.googleapis.com">
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
    <link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;600&family=Manrope:wght@400;500;700;800&family=Syne:wght@500;700;800&display=swap" rel="stylesheet">
    <link rel="stylesheet" href="/dashboard/landing.css">
</head>
<body>
<div class="scene" aria-hidden="true">
    <div class="orb orb-a"></div>
    <div class="orb orb-b"></div>
    <div class="orb orb-c"></div>
</div>

<header class="nav-shell reveal">
    <nav class="container nav">
        <a href="/" class="logo" aria-label="RustQueue home">
            <span class="logo-mark"></span>
            <span class="logo-text">RustQueue</span>
        </a>
        <div class="nav-links">
            <a href="#how-it-works">How It Works</a>
            <a href="#features">Features</a>
            <a href="#get-started">Get Started</a>
        </div>
        <a class="btn btn-ghost" href="/dashboard">Open Dashboard</a>
    </nav>
</header>

<main>
    <section class="hero container">
        <div class="hero-copy reveal">
            <p class="eyebrow">No Redis. No RabbitMQ. No external services.</p>
            <h1>Background Jobs <span>Without Infrastructure</span></h1>
            <p class="lead">
                Add persistent, crash-safe background job processing to any Rust application.
                Three lines of code. Zero operational overhead.
            </p>
            <div class="hero-code">
                <pre><code><span class="kw">let</span> rq = RustQueue::redb(<span class="str">"./jobs.db"</span>)?.build()?;
rq.push(<span class="str">"emails"</span>, <span class="str">"send-welcome"</span>, json!({<span class="str">"to"</span>: <span class="str">"a@b.com"</span>}), None).<span class="kw">await</span>?;

<span class="comment">// That's it. The job is persisted. It survives crashes.</span></code></pre>
            </div>
            <div class="hero-cta">
                <a class="btn btn-primary" href="#get-started">Get Started</a>
                <a class="btn btn-ghost" href="https://github.com/ferax564/rustqueue">View Source</a>
            </div>
        </div>

        <div class="hero-side">
            <article class="panel panel-stats reveal">
                <h2>Single Binary</h2>
                <div class="kpis">
                    <div>
                        <span class="mono">binary</span>
                        <strong>6.8 MB</strong>
                    </div>
                    <div>
                        <span class="mono">startup</span>
                        <strong>10 ms</strong>
                    </div>
                    <div>
                        <span class="mono">idle RAM</span>
                        <strong>15 MB</strong>
                    </div>
                </div>
            </article>
            <article class="panel panel-compare reveal">
                <h2>Setup Comparison</h2>
                <ul class="compare-list">
                    <li><strong>RustQueue</strong> <code>cargo add rustqueue</code></li>
                    <li><span class="muted">BullMQ</span> <code>Install Redis + npm i bullmq</code></li>
                    <li><span class="muted">Celery</span> <code>Install Redis/RabbitMQ + pip install celery</code></li>
                </ul>
            </article>
        </div>
    </section>

    <section id="how-it-works" class="how-it-works container">
        <div class="section-head reveal">
            <p class="eyebrow">How it works</p>
            <h2>Start Embedded. Grow Into a Server.</h2>
        </div>
        <div class="growth-path">
            <article class="growth-step reveal">
                <div class="step-num">1</div>
                <h3>Import the Library</h3>
                <p>Add <code>rustqueue</code> to your Cargo.toml. Use it directly in your app — no server process, no network, no config file.</p>
                <pre class="code-sm"><code>let rq = RustQueue::redb("./jobs.db")?.build()?;
rq.push("emails", "welcome", data, None).await?;</code></pre>
            </article>
            <article class="growth-step reveal">
                <div class="step-num">2</div>
                <h3>Run as a Server</h3>
                <p>When you need language-agnostic access, run the same engine as a standalone server. Same data file, same guarantees.</p>
                <pre class="code-sm"><code>rustqueue serve --storage ./jobs.db</code></pre>
            </article>
            <article class="growth-step reveal">
                <div class="step-num">3</div>
                <h3>Scale With SDKs</h3>
                <p>Connect workers in any language. Node.js, Python, and Go SDKs included — zero runtime dependencies each.</p>
                <pre class="code-sm"><code>const rq = new RustQueueClient("http://localhost:6790");
await rq.push("emails", "welcome", { to: "user@a.com" });</code></pre>
            </article>
        </div>
    </section>

    <section id="features" class="features container">
        <div class="section-head reveal">
            <p class="eyebrow">Batteries included</p>
            <h2>Everything You Need, Nothing You Don't</h2>
        </div>
        <div class="feature-grid">
            <article class="card reveal">
                <h3>Crash-Safe Storage</h3>
                <p>ACID-compliant embedded database. Jobs persist automatically. Safe to kill -9 at any time.</p>
            </article>
            <article class="card reveal">
                <h3>Retries + Backoff + DLQ</h3>
                <p>Fixed, linear, or exponential backoff. Failed jobs land in a dead-letter queue for inspection.</p>
            </article>
            <article class="card reveal">
                <h3>Cron + Interval Scheduling</h3>
                <p>Built-in schedule engine. Cron expressions and fixed intervals with pause/resume.</p>
            </article>
            <article class="card reveal">
                <h3>DAG Workflows</h3>
                <p>Job dependencies with cycle detection and cascade failure. Multi-step pipelines built in.</p>
            </article>
            <article class="card reveal">
                <h3>Webhooks + WebSocket</h3>
                <p>HMAC-signed callbacks on job events. Real-time WebSocket stream for live monitoring.</p>
            </article>
            <article class="card reveal">
                <h3>Observability</h3>
                <p>15+ Prometheus metrics, per-queue gauges, latency histograms. Pre-built Grafana dashboard.</p>
            </article>
        </div>
    </section>

    <section id="get-started" class="launch container reveal">
        <div>
            <p class="eyebrow">Get started</p>
            <h2>Your First Background Job in Two Minutes</h2>
        </div>
        <pre class="code"><code># As a library in your Rust project
cargo add rustqueue tokio serde_json anyhow

# Or as a standalone server
cargo install rustqueue && rustqueue serve

# Or with Docker
docker compose up -d</code></pre>
        <div class="launch-actions">
            <a class="btn btn-primary" href="/dashboard">Open Dashboard</a>
            <a class="btn btn-ghost" href="https://github.com/ferax564/rustqueue">View Source</a>
        </div>
    </section>
</main>

<footer class="footer container">
    <p>RustQueue &middot; Background jobs without infrastructure</p>
    <p class="mono">MIT OR Apache-2.0</p>
</footer>
</body>
</html>
```

- [ ] **Step 2: Add CSS for new hero code block and growth path**

Append the following to the end of `dashboard/static/landing.css`:

```css
/* ── Hero code block ────────────────────────────────────────────────── */
.hero-code {
    margin: 2rem 0;
    background: var(--ink);
    border-radius: var(--radius-sm);
    padding: 1.5rem 2rem;
    overflow-x: auto;
}

.hero-code pre {
    margin: 0;
}

.hero-code code {
    font-family: "JetBrains Mono", monospace;
    font-size: 0.95rem;
    line-height: 1.7;
    color: #e2e8f0;
}

.hero-code .kw { color: #c792ea; }
.hero-code .str { color: #c3e88d; }
.hero-code .comment { color: #637777; font-style: italic; }

/* ── Growth path ────────────────────────────────────────────────────── */
.how-it-works {
    padding: 6rem 0;
}

.growth-path {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 2rem;
    margin-top: 3rem;
}

.growth-step {
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    padding: 2rem;
}

.step-num {
    width: 2.5rem;
    height: 2.5rem;
    border-radius: 50%;
    background: var(--accent);
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 700;
    font-size: 1.1rem;
    margin-bottom: 1rem;
}

.code-sm {
    background: var(--ink);
    border-radius: var(--radius-sm);
    padding: 1rem 1.25rem;
    overflow-x: auto;
    margin-top: 1rem;
}

.code-sm code {
    font-family: "JetBrains Mono", monospace;
    font-size: 0.85rem;
    line-height: 1.6;
    color: #e2e8f0;
}

/* ── Compare list ───────────────────────────────────────────────────── */
.compare-list {
    list-style: none;
    padding: 0;
    margin: 0;
}

.compare-list li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--line);
    font-size: 0.9rem;
}

.compare-list li:last-child {
    border-bottom: none;
}

.compare-list .muted {
    color: var(--muted);
}

.compare-list code {
    font-family: "JetBrains Mono", monospace;
    font-size: 0.8rem;
    color: var(--muted);
}
```

- [ ] **Step 3: Verify the landing page renders**

Run: `cargo run -- serve` (start the server, visit `http://localhost:6790/` in browser)

Verify:
- Hero headline reads "Background Jobs Without Infrastructure"
- Embedded Rust code block is the visual centerpiece
- No benchmark table visible
- "How It Works" section shows 3-step growth path
- Setup comparison panel shows RustQueue vs BullMQ vs Celery on ease-of-setup
- Feature cards reduced from 11 to 6 (focused, no redundancy)

- [ ] **Step 4: Commit**

```bash
git add dashboard/static/landing.html dashboard/static/landing.css
git commit -m "site: rewrite landing page — embedded-first hero, growth path

Replaces 'Faster than RabbitMQ' hero with 'Background Jobs Without
Infrastructure'. Leading visual is now a Rust code block showing 3-line
embedded usage. New 'How It Works' section shows embedded -> server ->
SDKs growth path. Feature cards reduced from 11 to 6."
```

---

## Task 8: Update Cargo.toml Description and Keywords

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Update the package metadata**

In `Cargo.toml`, change the description line:

From:
```toml
description = "A high-performance distributed job scheduler — zero dependencies, single binary"
```

To:
```toml
description = "Background jobs without infrastructure — embeddable job queue with zero external dependencies"
```

Change the keywords:

From:
```toml
keywords = ["job-queue", "scheduler", "distributed", "async", "task"]
```

To:
```toml
keywords = ["job-queue", "background-jobs", "embedded", "async", "task-queue"]
```

Change the categories:

From:
```toml
categories = ["asynchronous", "web-programming"]
```

To:
```toml
categories = ["asynchronous", "web-programming", "database-implementations"]
```

- [ ] **Step 2: Verify metadata is valid**

Run: `cargo check`

Expected: compiles without errors.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: update crate metadata — embedded-first positioning

Description now leads with 'background jobs without infrastructure'.
Keywords updated: 'distributed' -> 'embedded', 'scheduler' -> 'background-jobs'.
Added 'database-implementations' category to signal embedded nature."
```

---

## Task 9: Update lib.rs Module Doc

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Rewrite the crate-level doc comment**

Replace the top doc comment in `src/lib.rs`:

From:
```rust
//! RustQueue — A high-performance distributed job scheduler.
//!
//! RustQueue is a zero-dependency, single-binary job queue and task scheduler
//! that can be used as a standalone server or embedded as a library.
```

To:
```rust
//! # RustQueue — Background jobs without infrastructure
//!
//! Add persistent, crash-safe background job processing to any Rust application.
//! No Redis. No RabbitMQ. No external services.
//!
//! ```no_run
//! use rustqueue::RustQueue;
//! use serde_json::json;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let rq = RustQueue::redb("./jobs.db")?.build()?;
//! let id = rq.push("emails", "send-welcome", json!({"to": "a@b.com"}), None).await?;
//! let jobs = rq.pull("emails", 1).await?;
//! rq.ack(jobs[0].id, None).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Jobs persist to an embedded ACID database. They survive crashes and restarts.
//! When you outgrow embedded mode, run the same engine as a standalone server
//! with `rustqueue serve`.
```

- [ ] **Step 2: Verify docs compile**

Run: `cargo doc --no-deps`

Expected: builds without warnings.

- [ ] **Step 3: Commit**

```bash
git add src/lib.rs
git commit -m "docs: rewrite crate-level doc — embedded-first with code example"
```

---

## Summary: What Changed and Why

| Before | After | Why |
|--------|-------|-----|
| "High-performance distributed job scheduler" | "Background jobs without infrastructure" | Exits the benchmark red ocean; speaks to non-customers who never considered a job queue |
| Benchmark table as README centerpiece | Embedded Rust code as README centerpiece | Entry point is a library import, not a performance comparison |
| "Faster than RabbitMQ" hero | "No Redis. No RabbitMQ. No external services." hero | Competes on setup experience, not throughput |
| 11 feature cards | 6 feature cards | Reduces cognitive load; hides advanced features behind progressive disclosure |
| No examples directory | 4 progressive examples (basic, persistent, worker, axum) | Primary onboarding path; each example is self-contained and runnable |
| No framework integration | `RqState` Axum extractor | "Add background jobs to your Axum app" becomes a 3-line change |
| "distributed" in keywords | "embedded" in keywords | Signals the new category on crates.io |
| Server-first "Get Started" | Library-first "Get Started" | First experience is `cargo add`, not `cargo install` |
| Performance section prominent | Performance section as reference link | Speed is real but not the pitch — it's a nice surprise, not the headline |

### What Was NOT Changed

- No code changes to the queue engine, storage backends, or protocol layers.
- No features removed — DAGs, webhooks, 5 backends all still exist and are documented.
- Benchmarks still exist in `docs/` and are linked from README.
- Server mode still documented and promoted as the growth path.
- This is a pure repositioning — same product, new frame.
