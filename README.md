# RustQueue

**The job scheduler that runs anywhere, scales to anything, and depends on nothing.**

A high-performance distributed job queue and task scheduler written in Rust. Zero external dependencies. Single binary. Runs from a Raspberry Pi to a 100-node cluster.

## Features

- **Zero dependencies** — No Redis, no PostgreSQL, no message broker. Just one binary.
- **Single binary deployment** — Download, run, done.
- **Multiple storage backends** — redb (default), hybrid memory+disk, SQLite, PostgreSQL, or in-memory
- **Job queuing** with priorities, delays, FIFO/LIFO ordering
- **DAG workflows** — Job dependencies with `depends_on`, cycle detection, cascade failure, and flow status tracking
- **Webhooks** — HTTP callbacks with HMAC-SHA256 signing, event/queue filtering, and retry delivery
- **Cron & interval scheduling** — Full schedule engine with cron expressions and interval-based execution
- **Automatic retries** with configurable backoff (fixed, linear, exponential)
- **Dead letter queues** for inspecting and retrying failed jobs
- **Job progress tracking** — Report progress (0-100%) with optional messages
- **Job timeouts & stall detection** — Background scheduler enforces deadlines and detects stalled workers
- **Worker heartbeats** — Workers signal liveness to prevent false stall detection
- **Queue pause/resume** — Pause queues to reject new jobs; resume when ready
- **Input validation** — Payload size limits, name sanitization, length restrictions
- **Real-time events** via WebSocket at `/api/v1/events`
- **Comprehensive Prometheus metrics** — 15+ metrics: counters, gauges (per-queue depth), histograms (latency)
- **OpenTelemetry** integration (optional, `--features otel`)
- **Grafana dashboard** — Pre-built dashboard JSON with starter Prometheus + Grafana stack
- **Bearer token authentication** — HTTP middleware + TCP connection-level auth
- **Auth rate limiting** — 5 failed attempts = 5-minute lockout per IP
- **TLS for TCP** — `rustls`-based encryption (optional, `--features tls`)
- **Graceful shutdown** — Connection draining with 30s timeout on `Ctrl+C`
- **Retention auto-cleanup** — Configurable TTLs for completed, failed, and DLQ jobs
- **Embedded web dashboard** — Overview, queues, schedules, DLQ, live events at `/dashboard`
- **Landing page** — Marketing page at `/` with feature showcase
- **CORS + Request tracing** — Cross-origin support and structured HTTP request spans
- **Embeddable** — Use as a standalone server or as a Rust library with zero config
- **Client SDKs** — Official Node.js (TypeScript), Python, and Go SDKs
- **CLI management** — `status`, `push`, `inspect`, `schedules` commands for operating a running server
- **Docker ready** — Dockerfile + Docker Compose with optional Prometheus/Grafana monitoring
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
| **Hybrid** (memory+disk) | always compiled | High throughput — DashMap hot path, periodic redb snapshots |
| **In-Memory** (DashMap) | always compiled | Testing, ephemeral queues, development |
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
backend = "redb"       # Options: "redb", "hybrid", "in_memory", "sqlite", "postgres"
path = "./data"
redb_durability = "immediate"  # "none" (unsafe fastest), "eventual", or "immediate" (safest)
write_coalescing_enabled = false    # Buffer single push/ack into batched flushes (huge throughput boost)
write_coalescing_interval_ms = 10   # Flush interval (ms)
write_coalescing_max_batch = 100    # Max buffered ops before forced flush
hybrid_snapshot_interval_ms = 1000  # How often hybrid storage flushes to disk
hybrid_max_dirty = 5000             # Max dirty entries before forced flush

[jobs]
default_max_attempts = 3
default_backoff = "exponential"
default_backoff_delay_ms = 1000
stall_timeout_ms = 30000
max_dag_depth = 10                  # Maximum DAG dependency chain depth

[webhooks]
enabled = false
delivery_timeout_ms = 10000
max_retries = 3
retry_base_delay_ms = 1000

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
# Build and run standalone
docker compose up -d

# Run with Prometheus + Grafana monitoring
docker compose -f docker-compose.monitoring.yml up -d

# Access points:
#   RustQueue API:     http://localhost:6790
#   Dashboard:         http://localhost:6790/dashboard
#   Prometheus:        http://localhost:9090
#   Grafana:           http://localhost:3000 (admin/admin)
```

See `deploy/` for production config templates (rustqueue.toml, prometheus.yml, grafana provisioning).

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
POST   /api/v1/queues/{queue}/pause   # Pause queue (rejects pushes with 503)
POST   /api/v1/queues/{queue}/resume  # Resume paused queue
GET    /api/v1/health                  # Health check
GET    /api/v1/metrics/prometheus      # Prometheus metrics
GET    /api/v1/queues/{queue}/dlq      # List dead letter queue jobs
POST   /api/v1/schedules              # Create schedule (cron or interval)
GET    /api/v1/schedules              # List all schedules
GET    /api/v1/schedules/{name}       # Get schedule by name
DELETE /api/v1/schedules/{name}       # Delete schedule
POST   /api/v1/schedules/{name}/pause  # Pause schedule
POST   /api/v1/schedules/{name}/resume # Resume schedule
POST   /api/v1/webhooks               # Register webhook
GET    /api/v1/webhooks               # List webhooks
GET    /api/v1/webhooks/{id}          # Get webhook
DELETE /api/v1/webhooks/{id}          # Delete webhook
GET    /api/v1/flows/{flow_id}        # Flow status (DAG jobs + summary)
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

    // Or hybrid (in-memory speed + disk durability via periodic snapshots)
    // let rq = RustQueue::hybrid("./data")?.build()?;

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

**RustQueue beats RabbitMQ** on produce, consume, and end-to-end throughput (hybrid TCP, batch_size=50):

| System | Produce ops/s | Consume ops/s | End-to-end ops/s |
|--------|------------:|------------:|-----------------:|
| **RustQueue TCP** | **43,494** | **14,195** | **14,681** |
| RabbitMQ | 35,975 | 2,675 | 2,902 |
| Redis (LPUSH/RPOP) | 5,460 | 4,673 | 2,346 |
| BullMQ | 4,761 | 5,130 | 845 |

| Metric | Target | Current (v0.13) |
|--------|--------|----------------|
| Throughput (push, hybrid TCP batch_size=50) | >= 50,000 jobs/sec | **~43,494/sec** |
| Throughput (push, hybrid TCP batch_size=1) | >= 30,000 jobs/sec | **~23,382/sec** |
| Throughput (end-to-end, hybrid TCP batch_size=50) | >= 10,000 jobs/sec | **~14,681/sec** |
| Throughput (consume, hybrid TCP batch_size=1) | >= 10,000 jobs/sec | **~10,987/sec** |
| Memory (idle) | < 20 MB | ~15 MB |
| Binary size | < 15 MB | 6.8 MB |
| Startup time | < 500 ms | ~10 ms |

> **v0.13 highlights:**
> - **Webhooks**: HMAC-SHA256 signed HTTP callbacks with event/queue filtering and retry delivery.
> - **DAG Flows**: Job dependencies with `depends_on`, BFS cycle detection, cascade DLQ failure, flow status endpoint.
> - **Beats RabbitMQ**: 1.2x faster produce, 5.3x faster consume, 5.1x faster end-to-end.
> - **TCP pipelining**: Reads all buffered commands before processing, single flush per batch.
> - **Per-queue dequeue index**: BTreeSet-based waiting index for O(log N) dequeue.
> - See `docs/performance-analysis.md` and `docs/competitor-benchmark-2026-02-07.md` for details.

## Client SDKs

### Node.js (TypeScript)

```bash
npm install @rustqueue/client
```

```typescript
import { RustQueueClient, RustQueueTcpClient } from "@rustqueue/client";

// HTTP client (simpler, good for most use cases)
const http = new RustQueueClient({ baseUrl: "http://localhost:6790" });
const jobId = await http.push("emails", "send-welcome", { to: "alice@example.com" });

// TCP client (lower overhead, for high-throughput workers)
const tcp = new RustQueueTcpClient({ host: "127.0.0.1", port: 6789 });
await tcp.connect();
const jobs = await tcp.pull("emails");
await tcp.ack(jobs[0].id);
tcp.disconnect();
```

### Python

```bash
pip install rustqueue
```

```python
from rustqueue import RustQueueClient

client = RustQueueClient("http://localhost:6790")
job_id = client.push("emails", "send-welcome", {"to": "alice@example.com"})
jobs = client.pull("emails")
client.ack(jobs[0]["id"])
```

### Go

```go
import "github.com/rustqueue/rustqueue-go/rustqueue"

// HTTP client
client := rustqueue.NewClient("http://localhost:6790", "")
jobID, _ := client.Push("emails", "send-welcome", map[string]any{"to": "alice@example.com"})
jobs, _ := client.Pull("emails", 1)
client.Ack(jobs[0].ID, nil)

// TCP client (higher throughput)
tcp, _ := rustqueue.NewTCPClient("127.0.0.1:6789", "")
tcp.Push("emails", "send-welcome", map[string]any{"to": "alice@example.com"}, nil)
tcp.Close()
```

All three SDKs support: push, pull, ack, fail, cancel, progress, heartbeat, batch operations, schedule CRUD, queue stats, DLQ, and health checks. See `sdk/node/examples/`, `sdk/python/examples/`, and `sdk/go/examples/` for comprehensive examples.

## Roadmap Priorities (2026)

- **P0:** phase 6 distributed mode (Raft/failover) and ForgePipe workflow metadata primitives.
- **P1:** phase 5b throughput backlog (hybrid memory+disk, sharding path) and batch-first client paths.
- **P2:** post-v1 multi-tenant controls, audit log, and flow-builder UX.

RustQueue is the organization scheduler and orchestration spine for ForgePipe and engine workflows.

See `ROADMAP.md` for the synchronized project roadmap.

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
