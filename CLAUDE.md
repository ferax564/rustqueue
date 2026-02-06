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
│   ├── mod.rs        # StorageBackend trait (20 async methods)
│   ├── redb.rs       # redb embedded backend (default, always compiled)
│   ├── buffered_redb.rs # Write-coalescing wrapper around RedbStorage
│   ├── memory.rs     # In-memory backend (always compiled, great for tests)
│   ├── sqlite.rs     # SQLite backend (behind `sqlite` feature)
│   └── postgres.rs   # PostgreSQL backend (behind `postgres` feature)
├── protocol/         # TCP protocol: newline-delimited JSON
├── config/           # Configuration loading: TOML + env + CLI
└── dashboard/        # Embedded web dashboard (rust-embed)
```

## Key Design Decisions

- **Storage trait**: All backends implement `StorageBackend` (20 async methods, see `src/storage/mod.rs`). Swap redb/sqlite/postgres/memory via config without changing engine code.
- **Dual protocol**: HTTP (port 6790) for general use + TCP (port 6789) for high-throughput workers. Both support identical operations.
- **Crash-only design**: All state is persisted before acknowledging writes. Safe to `kill -9` at any time.
- **UUID v7 for job IDs**: Time-sortable, globally unique, no coordination needed.
- **Feature flags**: Optional backends and integrations behind Cargo features (`sqlite`, `postgres`, `otel`, `cli`, `tls`). Only `cli` is default.
- **Embeddable library**: `RustQueue::memory().build()` or `RustQueue::redb(path).build()` for zero-config use without a server.
- **Background scheduler**: Tick loop (configurable interval) handles delayed job promotion, schedule execution (cron + interval), timeout detection, stall detection, and retention cleanup.
- **WebSocket events**: Real-time job lifecycle events via `tokio::sync::broadcast` channel (capacity 1024).
- **Authentication**: Bearer token auth for HTTP (public/protected route split) and TCP (connection-level handshake). Configurable via `[auth]` TOML section.
- **Graceful shutdown**: `Ctrl+C` triggers coordinated drain with 30s timeout for HTTP, TCP, and scheduler.
- **Embedded dashboard**: SPA served via `rust-embed` at `/dashboard` (overview, queues, DLQ, live events). Landing page at `/`.

## Known Performance Limitations

The default redb backend has ~348 push/sec sequential throughput. With `BufferedRedbStorage` write coalescing enabled (v0.10), concurrent push throughput reaches **~22,222 jobs/sec at 100 concurrent callers** (60.6x improvement, within 2.3x of 50K target). Batched TCP mode reaches ~10.9K push/sec, ~3.7K push+pull+ack cycles/sec.

v0.10 improvements:

1. **BufferedRedbStorage**: Automatic write coalescing for single-job `push`/`ack` flows. Enable via `write_coalescing_enabled = true` in `[storage]`.
2. **Unique key index**: O(1) lookup for `get_job_by_unique_key()` via `JOBS_UNIQUE_KEY_INDEX`.
3. **Index-based cleanup**: `remove_*_before()` methods use `JOBS_STATE_UPDATED_INDEX` prefix scan instead of full table scan.
4. **Queue names from index**: `list_queue_names()` extracts from index keys without JSON deserialization.

Remaining bottlenecks:

1. **Per-transition durability cost**: lifecycle operations still commit write transactions frequently (mitigated by write coalescing)
2. **Write amplification on state changes**: v0.6 indexes mean transitions update main row + 3 index rows in one transaction
3. **Protocol overhead**: single-job TCP path is strict request/response JSON per command (batch commands now exist but are opt-in)
4. **Durability trade-off is configurable**: `storage.redb_durability` supports `none`, `eventual`, and `immediate`

References:

- `docs/performance-analysis.md`
- `docs/competitor-benchmark-2026-02-06.md`
- `docs/competitor-benchmark-2026-02-06-immediate-r2-latest.md`
- `docs/competitor-gap-analysis-2026-02-06.md`

Immediate next steps:

1. Re-run competitor suite with `--write-coalescing` to measure actual single-job improvement.
2. Expand SDK/client helpers so batch commands are the default high-throughput path.
3. Re-run competitor suite on fixed control (`batch_size=1`) and batch (`batch_size=50`) profiles for trend tracking.

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
- **Generic backend harness**: `tests/storage_backend_tests.rs` — `backend_tests!` macro generates 18 canonical tests per storage backend.
- **Property tests**: State machine transitions, serialization roundtrips.
- **Benchmarks**: `benches/throughput.rs` measures jobs/sec for push, pull, ack operations.
- **Current counts**: ~183 tests (default features), ~199 tests (with `sqlite`).

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
GET    /dashboard                      # Embedded web dashboard
GET    /                               # Landing page
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
| `rust-embed` | Compile dashboard assets into binary |
| `reqwest` | CLI HTTP client (optional, `cli` feature) |
