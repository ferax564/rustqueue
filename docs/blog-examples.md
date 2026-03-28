# Blog Post Examples: "Background Jobs in Rust Without Redis"

Ready-to-use code snippets for the launch blog post. Each example is self-contained, compiles, and runs.

---

## The Hook — Three Lines

```rust
let rq = RustQueue::redb("./jobs.db")?.build()?;
rq.push("emails", "send-welcome", json!({"to": "user@a.com"}), None).await?;
// That's it. The job is persisted to disk. It survives crashes.
```

---

## Example 1: The "Hello World" (Full, Runnable)

**Use for:** Opening section — "Your first background job in 60 seconds."

```rust
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("./jobs.db")?.build()?;

    let id = rq.push("emails", "send-welcome",
        json!({"to": "user@example.com"}), None).await?;
    println!("Queued: {id}");

    let jobs = rq.pull("emails", 1).await?;
    println!("Processing: {}", jobs[0].name);
    rq.ack(jobs[0].id, None).await?;

    Ok(())
}
```

**Caption:** "No Redis. No RabbitMQ. No Docker. No config file. Just `cargo add rustqueue`."

---

## Example 2: Adding Background Jobs to an Existing Axum App

**Use for:** "What if you already have a web app?" section.

```rust
use axum::routing::post;
use axum::{Json, Router};
use rustqueue::axum_integration::RqState;
use rustqueue::RustQueue;
use serde_json::json;
use std::sync::Arc;

async fn enqueue_email(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let to = body["to"].as_str().unwrap_or("unknown");
    let id = rq.push("emails", "send-welcome", json!({"to": to}), None).await.unwrap();
    Json(json!({"queued": true, "job_id": id.to_string()}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = Arc::new(RustQueue::redb("./jobs.db")?.build()?);
    let app = Router::new()
        .route("/send-email", post(enqueue_email))
        .with_state(rq);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

**Caption:** "Three lines added to your existing Axum app. The `RqState` extractor handles everything."

---

## Example 3: The Comparison Table

**Use for:** "Why not just use Redis?" section.

| | RustQueue | Redis + BullMQ | RabbitMQ | Celery |
|---|---|---|---|---|
| External dependencies | **None** | Redis server | Erlang + RabbitMQ | Redis/RabbitMQ + Python |
| Time to first job | **~60 seconds** | 15–30 minutes | 15–30 minutes | 15–30 minutes |
| Deployment | `cargo add` or single binary | 2+ services | 2+ services | 3+ services |
| Binary size / footprint | **6.8 MB, 15 MB RAM** | N/A, ~100 MB+ | N/A, ~100 MB+ | N/A, ~200 MB+ |
| Data persists on crash | Yes (ACID) | Configurable | Yes | Depends on broker |

---

## Example 4: The Growth Story

**Use for:** "What happens when you outgrow embedded mode?" section.

```
Day 1: Library in your app
─────────────────────────
let rq = RustQueue::redb("./jobs.db")?.build()?;
rq.push("emails", "welcome", data, None).await?;

Day 30: Same data, standalone server
─────────────────────────────────────
$ rustqueue serve --storage ./jobs.db

Day 60: Multi-language workers
──────────────────────────────
// Node.js worker
const rq = new RustQueueClient("http://localhost:6790");
const [job] = await rq.pull("emails", 1);
await sendEmail(job.data.to);
await rq.ack(job.id);

# Python analytics worker
rq = RustQueueClient("http://localhost:6790")
jobs = rq.pull("analytics", count=10)
for job in jobs:
    process(job["data"])
    rq.ack(job["id"])
```

**Caption:** "You never migrate. You never re-architect. The same `jobs.db` file works at every stage."

---

## Example 5: The SQLite Analogy

**Use for:** Positioning paragraph.

> Think of RustQueue not as a replacement for RabbitMQ, but as a replacement for `tokio::spawn`.
>
> SQLite didn't win by competing with PostgreSQL on features. It won by being the database you never had to think about. RustQueue is the same idea, applied to job queues.
>
> Most applications don't need a message broker. They need a way to run tasks in the background that won't be lost if the process crashes. RustQueue gives you that as a library call — and grows into a full server when you're ready.

---

## Example 6: Worker with Retries and Progress

**Use for:** "Production-ready" section showing it's not a toy.

```rust
use rustqueue::{RustQueue, JobOptions, BackoffStrategy};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::redb("./jobs.db")?.build()?;

    // Push with retry policy
    rq.push("reports", "generate-monthly", json!({"month": "march"}), Some(JobOptions {
        max_retries: Some(3),
        backoff: Some(BackoffStrategy::Exponential),
        timeout_ms: Some(30_000),
        ..Default::default()
    })).await?;

    // Worker loop
    loop {
        let jobs = rq.pull("reports", 5).await?;
        for job in &jobs {
            rq.update_progress(job.id, 10, Some("Starting...".into())).await?;

            // Do the actual work...
            tokio::time::sleep(Duration::from_secs(2)).await;

            rq.update_progress(job.id, 90, Some("Almost done...".into())).await?;
            rq.ack(job.id, Some(json!({"rows": 15420}))).await?;
        }
        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
```

**Caption:** "Retries, exponential backoff, progress tracking, timeouts — all built in. No extra infrastructure."

---

## Example 7: Performance Numbers (for benchmark section)

> **Hybrid storage (in-memory + disk snapshots):**
> - 40,504 produce ops/sec via TCP
> - 26,716 consume ops/sec via TCP
> - 18,810 end-to-end ops/sec
>
> Competitive with RabbitMQ on produce. **5.3x faster on consume. 4.5x faster end-to-end.**
>
> And that's with a 6.8 MB binary using 15 MB of RAM.

---

## Suggested Blog Post Outline

1. **Hook:** "What if background jobs didn't require infrastructure?"
2. **The 3-line example** (Example 1)
3. **The problem:** Redis + BullMQ/Sidekiq requires running a whole server just for background jobs
4. **The comparison table** (Example 3)
5. **Adding to an existing Axum app** (Example 2)
6. **The growth story** (Example 4) — embedded → server → multi-language
7. **The SQLite analogy** (Example 5)
8. **Production features** (Example 6) — retries, progress, timeouts
9. **Performance** (Example 7) — "fast is a nice surprise, not the headline"
10. **Get started:** `cargo add rustqueue tokio serde_json anyhow`

---

## Suggested Blog Title Options

1. "Background Jobs in Rust Without Redis"
2. "I Replaced Redis with Three Lines of Rust"
3. "The SQLite of Job Queues"
4. "RustQueue: Background Jobs Without Infrastructure"

**SEO target:** "rust background jobs", "job queue without redis", "embedded job queue rust"
