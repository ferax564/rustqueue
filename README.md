# RustQueue

**The job scheduler that runs anywhere, scales to anything, and depends on nothing.**

A high-performance distributed job queue and task scheduler written in Rust. Zero external dependencies. Single binary. Runs from a Raspberry Pi to a 100-node cluster.

## Features

- **Zero dependencies** — No Redis, no PostgreSQL, no message broker. Just one binary.
- **Single binary deployment** — Download, run, done.
- **Job queuing** with priorities, delays, FIFO/LIFO ordering
- **Cron scheduling** for recurring tasks
- **Automatic retries** with configurable backoff (fixed, linear, exponential)
- **Dead letter queues** for inspecting and retrying failed jobs
- **Job dependencies** (DAG-based workflows)
- **Real-time events** via WebSocket and Server-Sent Events
- **Built-in web dashboard** — No separate UI to deploy
- **Prometheus metrics** out of the box
- **Distributed mode** with Raft consensus for high availability
- **Embeddable** — Use as a standalone server or as a Rust library
- **Language-agnostic** — HTTP REST API + TCP protocol works with any language

## Quick Start

```bash
# Install
cargo install rustqueue

# Run
rustqueue serve

# Push a job (HTTP)
curl -X POST http://localhost:6790/api/v1/queues/emails/jobs \
  -H "Content-Type: application/json" \
  -d '{"name": "send-welcome", "data": {"to": "user@example.com"}}'

# Open the dashboard
open http://localhost:6790/dashboard
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
│  ┌──────────▼──────────────────┐                │
│  │     Storage (trait-based)   │                │
│  │  redb │ SQLite │ PostgreSQL │                │
│  └─────────────────────────────┘                │
│                                                 │
│  Raft Consensus (optional cluster mode)         │
│  Embedded Web Dashboard                         │
└────────────────────────────────────────────────┘
```

## Configuration

RustQueue works out of the box with zero configuration. For customization:

```toml
# rustqueue.toml
[server]
host = "0.0.0.0"
http_port = 6790
tcp_port = 6789

[storage]
backend = "redb"
path = "./data"

[auth]
enabled = false
tokens = ["your-secret-token"]

[jobs]
default_max_attempts = 3
default_backoff = "exponential"
default_backoff_delay_ms = 1000
```

Environment variables are also supported with the `RUSTQUEUE_` prefix:

```bash
RUSTQUEUE_HTTP_PORT=6790
RUSTQUEUE_STORAGE_BACKEND=redb
RUSTQUEUE_AUTH_TOKENS=token1,token2
```

Priority: CLI flags > Environment variables > Config file > Defaults

## Docker

```bash
docker run -p 6789:6789 -p 6790:6790 ghcr.io/rustqueue/rustqueue
```

## API

### HTTP REST

```
POST   /api/v1/queues/{queue}/jobs    # Push job(s)
GET    /api/v1/queues/{queue}/jobs    # Pull next job
POST   /api/v1/jobs/{id}/ack         # Acknowledge completion
POST   /api/v1/jobs/{id}/fail        # Report failure
GET    /api/v1/jobs/{id}             # Get job details
GET    /api/v1/queues                # List queues
GET    /api/v1/health                # Health check
GET    /api/v1/metrics/prometheus    # Prometheus metrics
```

### TCP Protocol

Newline-delimited JSON on port 6789:

```json
→ {"cmd":"push","queue":"emails","name":"send-welcome","data":{"to":"a@b.com"}}
← {"ok":true,"id":"01JKXYZ..."}

→ {"cmd":"pull","queue":"emails","timeout":30000}
← {"ok":true,"job":{"id":"01JKXYZ...","name":"send-welcome","data":{"to":"a@b.com"}}}

→ {"cmd":"ack","id":"01JKXYZ..."}
← {"ok":true}
```

## Embedded Usage (Rust Library)

```rust
use rustqueue::{Job, Queue};

let queue = Queue::new("emails").await?;
queue.push(Job::new("emails", "send-welcome", json!({"to": "a@b.com"}))).await?;
```

## Performance Targets

| Metric | Target |
|--------|--------|
| Throughput (push) | >= 50,000 jobs/sec |
| Latency (p99 push) | < 5 ms |
| Memory (idle) | < 20 MB |
| Binary size | < 15 MB |
| Startup time | < 500 ms |

## Development

```bash
cargo test               # Run tests
cargo bench              # Run benchmarks
cargo clippy             # Lint
cargo fmt                # Format
```

## License

MIT OR Apache-2.0
