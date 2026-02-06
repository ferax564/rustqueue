# RustQueue

**The job scheduler that runs anywhere, scales to anything, and depends on nothing.**

A high-performance distributed job queue and task scheduler written in Rust. Zero external dependencies. Single binary. Runs from a Raspberry Pi to a 100-node cluster.

## Features

- **Zero dependencies** — No Redis, no PostgreSQL, no message broker. Just one binary.
- **Single binary deployment** — Download, run, done.
- **Multiple storage backends** — redb (default), SQLite, PostgreSQL, or in-memory
- **Job queuing** with priorities, delays, FIFO/LIFO ordering
- **Cron & interval scheduling** — Full schedule engine with cron expressions and interval-based execution
- **Automatic retries** with configurable backoff (fixed, linear, exponential)
- **Dead letter queues** for inspecting and retrying failed jobs
- **Job progress tracking** — Report progress (0-100%) with optional messages
- **Job timeouts & stall detection** — Background scheduler enforces deadlines and detects stalled workers
- **Worker heartbeats** — Workers signal liveness to prevent false stall detection
- **Real-time events** via WebSocket at `/api/v1/events`
- **Prometheus metrics** out of the box
- **OpenTelemetry** integration (optional, `--features otel`)
- **Bearer token authentication** — HTTP middleware + TCP connection-level auth
- **TLS for TCP** — `rustls`-based encryption (optional, `--features tls`)
- **Graceful shutdown** — Connection draining with 30s timeout on `Ctrl+C`
- **Retention auto-cleanup** — Configurable TTLs for completed, failed, and DLQ jobs
- **Embedded web dashboard** — Overview, queues, DLQ, live events at `/dashboard`
- **Landing page** — Marketing page at `/` with feature showcase
- **CORS + Request tracing** — Cross-origin support and structured HTTP request spans
- **Embeddable** — Use as a standalone server or as a Rust library with zero config
- **CLI management** — `status`, `push`, `inspect`, `schedules` commands for operating a running server
- **Language-agnostic** — HTTP REST API + TCP protocol works with any language

## Quick Start

```bash
# Install
cargo install rustqueue

# Run the server
rustqueue serve

# Push a job (HTTP)
curl -X POST http://localhost:6790/api/v1/queues/emails/jobs \
  -H "Content-Type: application/json" \
  -d '{"name": "send-welcome", "data": {"to": "user@example.com"}}'

# Push a job (CLI)
rustqueue push --queue emails --name send-welcome --data '{"to": "user@example.com"}'

# Check queue status
rustqueue status

# Inspect a job
rustqueue inspect <job-id>

# Create a cron schedule
rustqueue schedules create --name hourly-report --queue reports --job-name generate --cron "0 * * * *"

# List schedules
rustqueue schedules list
```

## Architecture

```
┌────────────────────────────────────────────────┐
│              RustQueue Binary                   │
│                                                 │
│  HTTP REST API (port 6790)                      │
│  TCP Protocol  (port 6789)                      │
│                                                 │
│  ┌─────────────────────────────┐                │
│  │        Core Engine          │                │
│  │  Queue Mgr · Scheduler      │                │
│  │  Worker Mgr · Flow Engine   │                │
│  │  DLQ Manager · Rate Limiter │                │
│  └──────────┬──────────────────┘                │
│             │                                   │
│  ┌──────────▼──────────────────────────┐        │
│  │     Storage (trait-based)           │        │
│  │  redb (+ BufferedRedb) │ SQLite │ PG│        │
│  └─────────────────────────────────────┘        │
│                                                 │
│  Raft Consensus (optional cluster mode)         │
│  Embedded Web Dashboard                         │
└────────────────────────────────────────────────┘
```

## Storage Backends

RustQueue supports multiple storage backends, selectable via configuration:

| Backend | Feature Flag | Use Case |
|---------|-------------|----------|
| **redb** (default) | always compiled | Production single-node, zero setup, ACID |
| **In-Memory** | always compiled | Testing, ephemeral queues, development |
| **SQLite** | `--features sqlite` | Single-node, familiar SQL, WAL mode |
| **PostgreSQL** | `--features postgres` | Multi-node, shared storage, `SELECT FOR UPDATE SKIP LOCKED` |

## Configuration

RustQueue works out of the box with zero configuration. For customization:

```toml
# rustqueue.toml
[server]
host = "0.0.0.0"
http_port = 6790
tcp_port = 6789

[storage]
backend = "redb"       # Options: "redb", "in_memory", "sqlite", "postgres"
path = "./data"
redb_durability = "immediate"  # "none" (unsafe fastest), "eventual", or "immediate" (safest)
write_coalescing_enabled = false    # Buffer single push/ack into batched flushes (huge throughput boost)
write_coalescing_interval_ms = 10   # Flush interval (ms)
write_coalescing_max_batch = 100    # Max buffered ops before forced flush

[jobs]
default_max_attempts = 3
default_backoff = "exponential"
default_backoff_delay_ms = 1000
stall_timeout_ms = 30000

[scheduler]
tick_interval_ms = 1000

[auth]
enabled = false
tokens = ["my-secret-token"]   # Bearer tokens for HTTP + TCP auth

[retention]
completed_ttl = "7d"           # Auto-remove completed jobs after 7 days
failed_ttl = "30d"             # Auto-remove failed jobs after 30 days
dlq_ttl = "90d"                # Auto-remove DLQ jobs after 90 days

[tls]
enabled = false
cert_path = "certs/server.crt"
key_path = "certs/server.key"

[logging]
level = "info"
format = "text"        # "text" or "json"
```

Environment variables are also supported with the `RUSTQUEUE_` prefix:

```bash
RUSTQUEUE_HTTP_PORT=6790
RUSTQUEUE_TCP_PORT=6789
RUSTQUEUE_STORAGE_BACKEND=redb
```

Priority: CLI flags > Environment variables > Config file > Defaults

## Docker

```bash
docker run -p 6789:6789 -p 6790:6790 ghcr.io/rustqueue/rustqueue
```

## API

### HTTP REST

```
POST   /api/v1/queues/{queue}/jobs      # Push job(s)
GET    /api/v1/queues/{queue}/jobs      # Pull next job(s)
POST   /api/v1/jobs/{id}/ack           # Acknowledge completion
POST   /api/v1/jobs/{id}/fail          # Report failure
POST   /api/v1/jobs/{id}/cancel        # Cancel a job
POST   /api/v1/jobs/{id}/progress      # Update progress (0-100)
POST   /api/v1/jobs/{id}/heartbeat     # Worker heartbeat
GET    /api/v1/jobs/{id}               # Get job details
GET    /api/v1/queues                  # List queues
GET    /api/v1/queues/{queue}/stats    # Queue statistics
GET    /api/v1/health                  # Health check
GET    /api/v1/metrics/prometheus      # Prometheus metrics
GET    /api/v1/queues/{queue}/dlq      # List dead letter queue jobs
POST   /api/v1/schedules              # Create schedule (cron or interval)
GET    /api/v1/schedules              # List all schedules
GET    /api/v1/schedules/{name}       # Get schedule by name
DELETE /api/v1/schedules/{name}       # Delete schedule
POST   /api/v1/schedules/{name}/pause  # Pause schedule
POST   /api/v1/schedules/{name}/resume # Resume schedule
GET    /api/v1/events                  # WebSocket event stream
GET    /dashboard                      # Embedded web dashboard
GET    /                               # Landing page
```

### TCP Protocol

Newline-delimited JSON on port 6789:

```json
→ {"cmd":"push","queue":"emails","name":"send-welcome","data":{"to":"a@b.com"}}
← {"ok":true,"id":"01JKXYZ..."}

→ {"cmd":"pull","queue":"emails","count":1}
← {"ok":true,"jobs":[{"id":"01JKXYZ...","name":"send-welcome","data":{"to":"a@b.com"}}]}

→ {"cmd":"ack","id":"01JKXYZ..."}
← {"ok":true}

→ {"cmd":"push_batch","queue":"emails","jobs":[{"name":"send-a","data":{"to":"a@b.com"}},{"name":"send-b","data":{"to":"b@b.com"}}]}
← {"ok":true,"ids":["01JK...","01JL..."]}

→ {"cmd":"ack_batch","ids":["01JK...","01JL..."]}
← {"ok":true,"acked":2,"failed":0,"results":[{"id":"01JK...","ok":true},{"id":"01JL...","ok":true}]}

→ {"cmd":"progress","id":"01JKXYZ...","progress":50,"message":"Processing..."}
← {"ok":true}

→ {"cmd":"heartbeat","id":"01JKXYZ..."}
← {"ok":true}

→ {"cmd":"auth","token":"my-secret-token"}
← {"ok":true}

→ {"cmd":"schedule_create","name":"hourly-report","queue":"reports","job_name":"generate","cron_expr":"0 * * * *"}
← {"ok":true}

→ {"cmd":"schedule_list"}
← {"ok":true,"schedules":[...]}
```

### WebSocket Events

Connect to `ws://localhost:6790/api/v1/events` to receive real-time job lifecycle events:

```json
{"event":"job.pushed","job_id":"01JKXYZ...","queue":"emails","timestamp":"2026-02-06T12:00:00Z"}
{"event":"job.completed","job_id":"01JKXYZ...","queue":"emails","timestamp":"2026-02-06T12:00:01Z"}
{"event":"job.failed","job_id":"01JKXYZ...","queue":"emails","timestamp":"2026-02-06T12:00:02Z"}
{"event":"job.cancelled","job_id":"01JKXYZ...","queue":"emails","timestamp":"2026-02-06T12:00:03Z"}
```

## Embedded Usage (Rust Library)

Use RustQueue as an embedded library with zero config — no server needed:

```rust
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // In-memory (great for tests)
    let rq = RustQueue::memory().build()?;

    // Or file-backed with redb
    // let rq = RustQueue::redb("./data")?.build()?;

    // Or with write coalescing for high-throughput concurrent workloads
    // use rustqueue::storage::BufferedRedbConfig;
    // let rq = RustQueue::redb("./data")?
    //     .with_write_coalescing(BufferedRedbConfig { interval_ms: 10, max_batch: 100 })
    //     .build()?;

    // Push a job
    let id = rq.push("emails", "send-welcome", json!({"to": "a@b.com"}), None).await?;

    // Pull and process
    let jobs = rq.pull("emails", 1).await?;
    for job in &jobs {
        // do work...
        rq.ack(job.id).await?;
    }

    Ok(())
}
```

## Feature Flags

```bash
cargo build                             # Default: redb + CLI
cargo build --features sqlite           # + SQLite backend
cargo build --features postgres         # + PostgreSQL backend
cargo build --features otel             # + OpenTelemetry tracing
cargo build --features sqlite,otel      # Combine as needed
cargo build --features tls              # + TLS for TCP protocol
cargo build --no-default-features       # Server only, no CLI commands
```

## Performance

| Metric | Target | Current (v0.10, redb) |
|--------|--------|----------------------|
| Throughput (push, 100 concurrent + write coalescing) | >= 50,000 jobs/sec | **~22,222/sec** |
| Throughput (push, sequential single-job) | >= 50,000 jobs/sec | ~348/sec |
| Throughput (push, batched TCP `batch_size=50`) | >= 50,000 jobs/sec | ~10,929/sec |
| Throughput (push+pull+ack, batched TCP `batch_size=50`) | >= 30,000 jobs/sec | ~3,692/sec |
| Latency (p50 push) | < 1 ms | ~2.9 ms |
| Memory (idle) | < 20 MB | ~15 MB |
| Binary size | < 15 MB | 6.8 MB |
| Startup time | < 500 ms | ~10 ms |

> **v0.10 highlight:** `BufferedRedbStorage` write coalescing delivers **22,222 jobs/sec at 100 concurrent callers** (60.6x faster than raw redb). Enable with `write_coalescing_enabled = true` in your config. The benefit scales with concurrency — 1.7x at 10 callers, 11x at 50, 60.6x at 100. See `docs/performance-analysis.md` for details.

## Development

```bash
cargo test                                    # Run tests (default features)
cargo test --features sqlite                  # Include SQLite backend tests
cargo test --features postgres                # Include PostgreSQL tests (needs TEST_POSTGRES_URL)
cargo bench                                   # Run throughput benchmarks
cargo clippy --features sqlite,postgres,otel  # Lint all features
cargo fmt                                     # Format code
```

## License

MIT OR Apache-2.0
