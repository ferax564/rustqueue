# RustQueue — Claude Code Project Guide

## Project Overview

RustQueue is a high-performance distributed job scheduler written in Rust. Zero external dependencies, single-binary deployment. See `PRD.md` for full product requirements.

## Quick Commands

```bash
cargo check                          # Type-check without building
cargo test                           # Run all tests (default features)
cargo test --features sqlite         # Run tests including SQLite backend
cargo test -- --nocapture            # Run tests with stdout visible
cargo build                          # Debug build
cargo build --release                # Release build (optimized, stripped)
cargo bench                          # Run benchmarks (criterion)
cargo clippy                         # Lint
cargo clippy --features sqlite,postgres,otel  # Lint all features
cargo fmt                            # Format code
```

## Architecture

```
src/
├── main.rs           # Binary: CLI (serve/status/push/inspect/schedules), server startup, scheduler spawn
├── lib.rs            # Library root: re-exports RustQueue, QueueManager, Job, etc.
├── builder.rs        # RustQueue builder for zero-config embeddable library usage
├── api/              # HTTP REST API (axum)
│   ├── mod.rs        # AppState, router composition (public vs protected routes)
│   ├── auth.rs       # Bearer token auth middleware (HTTP + config)
│   ├── jobs.rs       # Job CRUD + progress + heartbeat + DLQ list endpoints
│   ├── schedules.rs  # Schedule CRUD + pause/resume endpoints
│   ├── queues.rs     # Queue listing and stats
│   ├── health.rs     # Health check
│   ├── openapi.rs    # OpenAPI 3.1 spec generation (utoipa) + Scalar UI
│   ├── prometheus.rs # Prometheus metrics endpoint
│   └── websocket.rs  # WebSocket event streaming (/api/v1/events)
├── engine/           # Core business logic
│   ├── models.rs     # Data types: Job, Schedule, Worker, enums
│   ├── queue.rs      # QueueManager: push, pull, ack, fail, progress, heartbeat, timeouts, stalls, schedule CRUD + execution
│   ├── scheduler.rs  # Background tick loop: promote delayed, execute schedules, check timeouts, detect stalls
│   ├── metrics.rs    # Prometheus counter/gauge instrumentation
│   ├── error.rs      # RustQueueError (thiserror)
│   └── telemetry.rs  # OpenTelemetry OTLP integration (behind `otel` feature)
├── storage/          # Storage abstraction and backends
│   ├── mod.rs        # StorageBackend trait (23 async methods)
│   ├── redb.rs       # redb embedded backend (default, always compiled)
│   ├── buffered_redb.rs # Write-coalescing wrapper around RedbStorage
│   ├── hybrid.rs     # Hybrid memory+disk backend (DashMap hot path + redb snapshots)
│   ├── memory.rs     # In-memory backend (DashMap-based, always compiled, great for tests)
│   ├── sqlite.rs     # SQLite backend (behind `sqlite` feature)
│   └── postgres.rs   # PostgreSQL backend (behind `postgres` feature)
├── protocol/         # TCP protocol: newline-delimited JSON
├── config/           # Configuration loading: TOML + env + CLI
└── dashboard/        # Embedded web dashboard (rust-embed)
```

## Key Design Decisions

- **Storage trait**: All backends implement `StorageBackend` (23 async methods, see `src/storage/mod.rs`). Swap redb/sqlite/postgres/memory/hybrid via config without changing engine code.
- **Dual protocol**: HTTP (port 6790) for general use + TCP (port 6789) for high-throughput workers. Both support identical operations.
- **Crash-only design**: All state is persisted before acknowledging writes. Safe to `kill -9` at any time.
- **UUID v7 for job IDs**: Time-sortable, globally unique, no coordination needed.
- **Feature flags**: Optional backends and integrations behind Cargo features (`sqlite`, `postgres`, `otel`, `cli`, `tls`). Only `cli` is default.
- **Embeddable library**: `RustQueue::memory().build()`, `RustQueue::redb(path).build()`, or `RustQueue::hybrid(path).build()` for zero-config use without a server.
- **Input validation**: Max lengths enforced on queue names (256), job names (1024), data payload (1MB), unique keys (1024), error messages (10KB).
- **Queue pause/resume**: Paused queues reject new pushes with 503. HTTP + TCP endpoints.
- **Auth rate limiting**: 5 failed auth attempts = 5-minute lockout per IP via in-memory DashMap tracker.
- **Comprehensive metrics**: 15+ Prometheus metrics — counters, gauges (per-queue depth), histograms (push/pull/ack latency), HTTP request tracking.
- **Hybrid storage**: DashMap in-memory hot path with periodic background snapshot to redb. Configurable flush interval and dirty threshold.
- **Client SDKs**: Node.js (`sdk/node/`, TypeScript, HTTP + TCP, zero deps) and Python (`sdk/python/`, stdlib-only HTTP).
- **Docker deployment**: Multi-stage Dockerfile, docker-compose.yml (standalone), docker-compose.monitoring.yml (with Prometheus + Grafana).
- **Background scheduler**: Tick loop (configurable interval) handles delayed job promotion, schedule execution (cron + interval), timeout detection, stall detection, and retention cleanup.
- **WebSocket events**: Real-time job lifecycle events via `tokio::sync::broadcast` channel (capacity 1024).
- **Authentication**: Bearer token auth for HTTP (public/protected route split) and TCP (connection-level handshake). Configurable via `[auth]` TOML section.
- **Graceful shutdown**: `Ctrl+C` triggers coordinated drain with 30s timeout for HTTP, TCP, and scheduler.
- **Embedded dashboard**: SPA served via `rust-embed` at `/dashboard` (overview, queues, DLQ, live events). Landing page at `/`.

## Performance

**RustQueue beats RabbitMQ** on produce (1.2x), consume (5.3x), and end-to-end (5.1x) throughput with hybrid TCP backend (batch_size=50). Key numbers (February 7, 2026):

| Metric | ops/s |
|--------|------:|
| Hybrid TCP produce (batch_size=50) | **43,494** |
| Hybrid TCP consume (batch_size=50) | **14,195** |
| Hybrid TCP end-to-end (batch_size=50) | **14,681** |
| Hybrid TCP produce (batch_size=1) | **23,382** |
| Hybrid TCP consume (batch_size=1) | **10,987** |

The default redb backend has ~348 push/sec sequential throughput (fsync-dominated). Multiple performance tiers:

- **HybridStorage** (in-memory + disk snapshots): DashMap hot path + periodic redb flush. Per-queue BTreeSet waiting index for O(log N) dequeue. Trade-off: up to `snapshot_interval` of data loss on crash.
- **BufferedRedbStorage** (write coalescing): **~22,222 jobs/sec at 100 concurrent callers** (60.6x improvement). Enable with `write_coalescing_enabled = true`.

v0.12 TCP optimizations:

1. **TCP_NODELAY**: Disables Nagle's algorithm on accepted connections.
2. **BufWriter + flush**: Application-level write buffering with explicit flush for fewer syscalls.
3. **TCP pipelining**: Reads all buffered commands before processing, single flush per batch.
4. **Clone reduction**: Avoids unnecessary `cmd.clone()` in push handlers.
5. **estimate_json_size()**: Walks Value tree without allocation for fast payload validation.
6. **Stack-allocated index key**: `state_updated_key()` returns `[u8; 25]` instead of `Vec<u8>`.
7. **Eventual durability for hybrid**: Inner redb forced to `Eventual` mode (safe since hybrid accepts data loss).
8. **Per-queue BTreeSet waiting index**: HybridStorage dequeue uses BTreeSet index — O(log N) per job instead of O(total_jobs) scan. 37.6x consume throughput improvement.

References:

- `docs/performance-analysis.md`
- `docs/competitor-benchmark-2026-02-07.md`
- `docs/competitor-benchmark-2026-02-06.md`

## Conventions

- **Error handling**: Use `thiserror` for library errors, `anyhow` for binary/integration code. Storage trait methods return `anyhow::Result`.
- **Async**: Everything async via tokio. Use `#[async_trait]` for trait objects.
- **Serialization**: All public types derive `Serialize, Deserialize` via serde. JSON payloads use `serde_json::Value`.
- **Testing**: Unit tests in `#[cfg(test)]` modules within source files. Integration tests in `tests/`. Property-based tests with `proptest`. Benchmarks with `criterion` in `benches/`.
- **Naming**: snake_case for files/functions, PascalCase for types, SCREAMING_CASE for constants. Module names match their domain concept.

## Feature Flags

| Feature | Dependencies | Purpose |
|---------|-------------|---------|
| `cli` (default) | `reqwest` | CLI management commands (`status`, `push`, `inspect`) |
| `sqlite` | `rusqlite` (bundled) | SQLite storage backend |
| `postgres` | `sqlx` (with pg) | PostgreSQL storage backend |
| `otel` | `opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `tracing-opentelemetry` | OpenTelemetry tracing export |
| `tls` | `rustls`, `tokio-rustls`, `rustls-pemfile` | TLS encryption for TCP protocol |

## Testing Strategy

- **Unit tests**: Each module has `#[cfg(test)] mod tests` testing individual functions.
- **Integration tests**: `tests/` directory tests full server behavior (HTTP + TCP + WebSocket).
- **Generic backend harness**: `tests/storage_backend_tests.rs` — `backend_tests!` macro generates 18 canonical tests per storage backend (memory, redb, buffered_redb, hybrid; + sqlite with feature).
- **Property tests**: State machine transitions, serialization roundtrips.
- **Benchmarks**: `benches/throughput.rs` measures jobs/sec for push, pull, ack operations.
- **Current counts**: ~212 tests (default features), ~228 tests (with `sqlite`).

## Configuration Priority

CLI flags > Environment variables (`RUSTQUEUE_*`) > Config file (`rustqueue.toml`) > Defaults

## Ports

| Protocol | Default Port | Purpose |
|----------|-------------|---------|
| HTTP     | 6790        | REST API, WebSocket, Dashboard |
| TCP      | 6789        | High-throughput worker protocol |
| Raft     | 6800        | Cluster consensus (when enabled) |

## API Endpoints

### HTTP

```
POST   /api/v1/queues/{queue}/jobs      # Push job(s)
GET    /api/v1/queues/{queue}/jobs      # Pull next job(s)
POST   /api/v1/jobs/{id}/ack           # Acknowledge completion
POST   /api/v1/jobs/{id}/fail          # Report failure
POST   /api/v1/jobs/{id}/cancel        # Cancel a job
POST   /api/v1/jobs/{id}/progress      # Update job progress (0-100)
POST   /api/v1/jobs/{id}/heartbeat     # Send worker heartbeat
GET    /api/v1/jobs/{id}               # Get job details
GET    /api/v1/queues                  # List queues
GET    /api/v1/queues/{queue}/stats    # Queue statistics
POST   /api/v1/queues/{queue}/pause   # Pause queue (rejects pushes)
POST   /api/v1/queues/{queue}/resume  # Resume queue
GET    /api/v1/health                  # Health check
GET    /api/v1/metrics/prometheus      # Prometheus metrics
GET    /api/v1/queues/{queue}/dlq      # List DLQ jobs
POST   /api/v1/schedules              # Create/upsert schedule
GET    /api/v1/schedules              # List all schedules
GET    /api/v1/schedules/{name}       # Get schedule by name
DELETE /api/v1/schedules/{name}       # Delete schedule
POST   /api/v1/schedules/{name}/pause  # Pause schedule
POST   /api/v1/schedules/{name}/resume # Resume schedule
GET    /api/v1/events                  # WebSocket event stream
GET    /api/v1/openapi.json            # OpenAPI 3.1 spec (JSON)
GET    /api/v1/docs                    # Scalar API reference UI
GET    /dashboard                      # Embedded web dashboard
GET    /                               # Landing page (always public)
```

### TCP Commands

`push`, `push_batch`, `pull`, `ack`, `ack_batch`, `fail`, `cancel`, `progress`, `heartbeat`, `get`, `schedule_create`, `schedule_list`, `schedule_get`, `schedule_delete`, `schedule_pause`, `schedule_resume`

## CLI Commands

```bash
rustqueue serve [--config PATH] [--http-port PORT] [--tcp-port PORT]
rustqueue status [--host HOST] [--http-port PORT]
rustqueue push --queue NAME --name JOB_NAME [--data JSON]
rustqueue inspect JOB_ID [--host HOST] [--http-port PORT]
rustqueue schedules list [--host HOST] [--http-port PORT]
rustqueue schedules create --name NAME --queue QUEUE --job-name JOB [--cron EXPR] [--every-ms MS]
rustqueue schedules delete NAME
rustqueue schedules pause NAME
rustqueue schedules resume NAME
```

## Dependencies of Note

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP framework + WebSocket |
| `tokio` | Async runtime |
| `redb` | Embedded ACID storage (default backend) |
| `rusqlite` | SQLite storage (optional, `sqlite` feature) |
| `sqlx` | PostgreSQL storage (optional, `postgres` feature) |
| `croner` | Cron expression parsing |
| `opentelemetry` family | OTLP trace export (optional, `otel` feature) |
| `metrics` + `metrics-exporter-prometheus` | Prometheus metrics |
| `dashmap` | Lock-free concurrent HashMap (MemoryStorage, HybridStorage, auth rate limiter) |
| `rust-embed` | Compile dashboard assets into binary |
| `reqwest` | CLI HTTP client (optional, `cli` feature) |

## Client SDKs

| SDK | Path | Transport | Dependencies |
|-----|------|-----------|-------------|
| **Node.js** (TypeScript) | `sdk/node/` | HTTP (`fetch`) + TCP (`net.Socket`) | Zero runtime deps, Node.js >= 18 |
| **Python** | `sdk/python/` | HTTP (`urllib.request`) | Zero deps, Python >= 3.8 |
| **Go** | `sdk/go/` | HTTP (`net/http`) + TCP (`net.Conn`) | Zero deps, Go >= 1.21 |

Both SDKs cover: push, pull, ack, fail, cancel, progress, heartbeat, get_job, list_queues, queue_stats, DLQ, schedule CRUD, health.

## Docker Deployment

| File | Purpose |
|------|---------|
| `Dockerfile` | Multi-stage build (rust:1.85-slim builder, debian:bookworm-slim runtime) |
| `docker-compose.yml` | Standalone RustQueue with persistent volume |
| `docker-compose.monitoring.yml` | RustQueue + Prometheus + Grafana (auto-provisioned dashboard) |
| `deploy/rustqueue.toml` | Production config template |
| `deploy/prometheus.yml` | Prometheus scrape config |
| `deploy/grafana-*.yml` | Grafana datasource + dashboard provisioning |
