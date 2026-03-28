# RustQueue

**High-performance distributed job scheduler. Zero dependencies. Single binary.**

[![CI](https://github.com/ferax564/rustqueue/actions/workflows/ci.yml/badge.svg)](https://github.com/ferax564/rustqueue/actions/workflows/ci.yml)
[![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

## Features

- **Zero dependencies** — No Redis, no PostgreSQL, no message broker. One binary.
- **Multiple storage backends** — redb (default), hybrid memory+disk, SQLite, PostgreSQL, in-memory
- **DAG workflows** — Job dependencies with cycle detection and cascade failure
- **Cron & interval scheduling** — Full schedule engine with pause/resume
- **Webhooks & WebSocket events** — HMAC-SHA256 signed callbacks + real-time stream
- **Embeddable** — Use as a server or as a Rust library with `RustQueue::memory().build()`
- **Client SDKs** — TypeScript, Python, Go — zero runtime dependencies

## Quick Start

```bash
# Install and run
cargo install rustqueue
rustqueue serve

# Push a job
curl -X POST http://localhost:6790/api/v1/queues/emails/jobs \
  -H "Content-Type: application/json" \
  -d '{"name": "send-welcome", "data": {"to": "user@example.com"}}'

# Check status
rustqueue status
```

## Embedded Usage

```rust
use rustqueue::RustQueue;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rq = RustQueue::memory().build()?;
    let id = rq.push("emails", "send-welcome", json!({"to": "a@b.com"}), None).await?;
    let jobs = rq.pull("emails", 1).await?;
    rq.ack(jobs[0].id, None).await?;
    Ok(())
}
```

## Storage Backends

| Backend | Feature Flag | Use Case |
|---------|-------------|----------|
| **redb** (default) | always compiled | Production single-node, ACID |
| **Hybrid** | always compiled | High throughput — memory hot path + disk snapshots |
| **In-Memory** | always compiled | Testing, development |
| **SQLite** | `--features sqlite` | Single-node, familiar SQL |
| **PostgreSQL** | `--features postgres` | Multi-node, shared storage |

## Performance

Benchmarked against production RabbitMQ, Redis, and BullMQ (hybrid TCP, batch_size=50):

| System | Produce ops/s | Consume ops/s | End-to-end ops/s |
|--------|------------:|------------:|-----------------:|
| **RustQueue** | **43,494** | **14,195** | **14,681** |
| RabbitMQ | 35,975 | 2,675 | 2,902 |
| Redis | 5,460 | 4,673 | 2,346 |
| BullMQ | 4,761 | 5,130 | 845 |

6.8 MB binary. 10ms startup. ~15 MB idle memory.

## Docker

```bash
docker compose up -d                                    # Standalone
docker compose -f docker-compose.monitoring.yml up -d   # With Prometheus + Grafana
```

## Client SDKs

| SDK | Install | Transport |
|-----|---------|-----------|
| [Node.js](sdk/node/) | `npm install @rustqueue/client` | HTTP + TCP |
| [Python](sdk/python/) | `pip install rustqueue` | HTTP |
| [Go](sdk/go/) | `go get github.com/rustqueue/rustqueue-go` | HTTP + TCP |

## Documentation

- [API Reference](docs/orchestration-api.md) — Full HTTP + TCP API documentation
- [OpenAPI Spec](http://localhost:6790/api/v1/docs) — Interactive Scalar UI (when server is running)
- [Configuration](deploy/rustqueue.toml) — Production config template
- [Contributing](CONTRIBUTING.md) — Development setup and PR process

## Feature Flags

```bash
cargo build                        # Default: redb + CLI
cargo build --features sqlite      # + SQLite backend
cargo build --features postgres    # + PostgreSQL backend
cargo build --features otel        # + OpenTelemetry tracing
cargo build --features tls         # + TLS for TCP protocol
```

## Development

```bash
cargo test                           # Run all tests
cargo test --features sqlite         # Include SQLite backend
cargo clippy -- -D warnings          # Lint (zero warnings required)
cargo fmt --check                    # Format check
cargo bench                          # Throughput benchmarks
```

## License

MIT OR Apache-2.0
