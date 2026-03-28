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
