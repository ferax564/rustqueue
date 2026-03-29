# RustQueue — Claude Code Project Guide

## Project Overview

RustQueue provides background jobs without infrastructure. It's an embeddable job queue written in Rust — use as a library (`RustQueue::redb("./jobs.db")?.build()?`) or as a standalone server. Zero external dependencies, single-binary deployment.

**Positioning:** "The SQLite of job queues." Not a replacement for RabbitMQ — a replacement for `tokio::spawn`. Lead with the embedded library story, not benchmarks.

**Version:** 0.2.0 — published on [crates.io](https://crates.io/crates/rustqueue).

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
├── axum_integration.rs # RqState extractor for embedding RustQueue in Axum apps
├── api/              # HTTP REST API (axum)
│   ├── mod.rs        # AppState, router composition (public vs protected routes)
│   ├── auth.rs       # Bearer token auth middleware (HTTP + config)
│   ├── jobs.rs       # Job CRUD + progress + heartbeat + DLQ list + flow status endpoints
│   ├── schedules.rs  # Schedule CRUD + pause/resume endpoints
│   ├── queues.rs     # Queue listing and stats
│   ├── health.rs     # Health check
│   ├── openapi.rs    # OpenAPI 3.1 spec generation (utoipa) + Scalar UI
│   ├── prometheus.rs # Prometheus metrics endpoint
│   ├── webhooks.rs   # Webhook CRUD (register, list, get, delete)
│   └── websocket.rs  # WebSocket event streaming (/api/v1/events)
├── engine/           # Core business logic
│   ├── models.rs     # Data types: Job, Schedule, Worker, enums
│   ├── queue.rs      # QueueManager: push, pull, ack, fail, progress, heartbeat, timeouts, stalls, schedule CRUD + execution, DAG dependency resolution
│   ├── scheduler.rs  # Background tick loop: promote delayed, execute schedules, check timeouts, detect stalls, promote orphaned blocked jobs
│   ├── webhook.rs    # WebhookManager: HMAC-SHA256 signing, retry delivery, broadcast dispatcher
│   ├── metrics.rs    # Prometheus counter/gauge instrumentation
│   ├── error.rs      # RustQueueError (thiserror)
│   └── telemetry.rs  # OpenTelemetry OTLP integration (behind `otel` feature)
├── storage/          # Storage abstraction and backends
│   ├── mod.rs        # StorageBackend trait (24 async methods)
│   ├── redb.rs       # redb embedded backend (default, always compiled)
│   ├── buffered_redb.rs # Write-coalescing wrapper around RedbStorage
│   ├── hybrid.rs     # Hybrid memory+disk backend (DashMap hot path + redb snapshots)
│   ├── memory.rs     # In-memory backend (DashMap-based, always compiled, great for tests)
│   ├── sqlite.rs     # SQLite backend (behind `sqlite` feature)
│   └── postgres.rs   # PostgreSQL backend (behind `postgres` feature)
├── protocol/         # TCP protocol: newline-delimited JSON
├── config/           # Configuration loading: TOML + env + CLI
└── dashboard/        # Embedded web dashboard + blog + examples (rust-embed)

examples/
├── basic.rs              # Simplest push/pull/ack (in-memory)
├── persistent.rs         # File-backed queue surviving restarts
├── worker.rs             # Long-running worker loop
└── axum_background_jobs.rs # Axum web app with background job queue
```

## Website & Publishing

| Asset | Location | URL |
|-------|----------|-----|
| Landing page (embedded) | `dashboard/static/landing.html` | `http://localhost:6790/` |
| Blog post (embedded) | `dashboard/static/blog-background-jobs-without-redis.html` | `/blog/background-jobs-without-redis` |
| Examples page (embedded) | `dashboard/static/examples.html` | `/examples` |
| GitHub Pages site | `docs/index.html` | https://ferax564.github.io/rustqueue/ |
| GitHub Pages blog | `docs/blog/background-jobs-without-redis.html` | https://ferax564.github.io/rustqueue/blog/background-jobs-without-redis.html |
| GitHub Pages examples | `docs/examples.html` | https://ferax564.github.io/rustqueue/examples.html |
| crates.io | — | https://crates.io/crates/rustqueue |
| Blog examples/outline | `docs/blog-examples.md` | — |

**GitHub Pages** deploys from `docs/` via `.github/workflows/pages.yml` (triggers on `docs/**` changes). Pages HTML must be self-contained — inline all CSS (no `/dashboard/landing.css` references).

**crates.io publishing** via `.github/workflows/publish.yml` — triggers on `v*` tags. Requires `CARGO_REGISTRY_TOKEN` secret (already configured). Verifies Cargo.toml version matches the tag, runs tests, then publishes.

**Social media** content managed via the `automarketing` project at `../automarketing/`. Uses `x_direct` backend (X API v2 with OAuth 1.0a). Drafts go to `social/drafts/`, approved to `social/approved/`, posted via `python3 social/scripts/post.py`.

## Key Design Decisions

- **Storage trait**: All backends implement `StorageBackend` (24 async methods). Swap via config.
- **Dual protocol**: HTTP (6790) + TCP (6789). Both support identical operations.
- **Crash-only design**: All state persisted before ack. Safe to `kill -9`.
- **Embeddable library**: `RustQueue::memory().build()`, `.redb(path)`, or `.hybrid(path)` — zero-config, no server needed.
- **Axum integration**: `RqState` extractor (`src/axum_integration.rs`) — `FromRequestParts<Arc<RustQueue>>` with `Deref`. State is `Arc<RustQueue>`.
- **DAG Flows**: `depends_on` field, BFS cycle detection, cascade DLQ on parent fail.
- **Hybrid storage**: DashMap hot path + periodic redb snapshots. Trade-off: up to `snapshot_interval` data loss on crash.
- **Webhooks**: HMAC-SHA256 signing, configurable retry, event/queue filtering.

## Performance (v0.2.0, March 2026)

Competitor benchmarks (hybrid TCP, batch_size=50, 5000 ops, 3 repeats):

| System | Produce | Consume | End-to-end |
|--------|--------:|--------:|-----------:|
| **RustQueue TCP** | **47,129/s** | **38,048/s** | **22,685/s** |
| RabbitMQ | 47,588/s | 5,367/s | 4,686/s |
| Redis | 9,337/s | 9,511/s | 4,951/s |
| BullMQ | 7,559/s | 6,690/s | 1,978/s |
| Celery | 3,168/s | 1,589/s | 893/s |

Neck-and-neck with RabbitMQ on produce. 7.1x faster consume. 4.8x faster end-to-end.

Internal benchmarks: Hybrid 300K+ concurrent push ops/sec. Buffered redb 19K ops/sec at 100 callers. Raw redb ~314 push/s (fsync-dominated).

Reference: `docs/competitor-benchmark-2026-03-28.md`, `docs/benchmark-v0.2.0.md`

Competitor benchmarks require Docker (Redis + RabbitMQ + Node for BullMQ + Celery):
```bash
python3 scripts/benchmark_competitors.py --ops 5000 --repeats 3 --rustqueue-tcp-batch-size 50 --hybrid
```

## Conventions

- **Error handling**: `thiserror` for library, `anyhow` for binary. Storage returns `anyhow::Result`.
- **Async**: Everything async via tokio.
- **Testing**: Unit tests in `#[cfg(test)]`, integration in `tests/`, property-based with `proptest`.
- **Naming**: snake_case files/functions, PascalCase types, SCREAMING_CASE constants.
- **Positioning**: Always "background jobs without infrastructure", never "distributed job scheduler" or "high-performance". The embedded library is the entry point, server mode is the growth path.

## Feature Flags

| Feature | Purpose |
|---------|---------|
| `cli` (default) | CLI management commands |
| `sqlite` | SQLite storage backend |
| `postgres` | PostgreSQL storage backend |
| `otel` | OpenTelemetry tracing export |
| `tls` | TLS encryption for TCP protocol |
| `axum-integration` | `RqState` Axum extractor |

## Testing

- ~315 tests (default), ~342 (with `sqlite`).
- `tests/storage_backend_tests.rs` — `backend_tests!` macro generates 19 tests per backend.
- Dashboard tests assert on "Background Jobs" + "Without Infrastructure" (not old positioning).
- CLI help test asserts on "Background jobs without infrastructure".

## Release Process

1. Bump version in `Cargo.toml`, `sdk/node/package.json`, `sdk/python/pyproject.toml`, `sdk/python/rustqueue/__init__.py`, `src/api/openapi.rs`
2. Run full test suite: `cargo test`
3. Commit, tag: `git tag -a vX.Y.Z -m "vX.Y.Z — description"`
4. Push: `git push origin main && git push origin vX.Y.Z`
5. CI auto-publishes to crates.io and creates GitHub Release

## Client SDKs

| SDK | Path | Transport | Version |
|-----|------|-----------|---------|
| **Node.js** (TypeScript) | `sdk/node/` | HTTP + TCP | 0.2.0 |
| **Python** | `sdk/python/` | HTTP | 0.2.0 |
| **Go** | `sdk/go/` | HTTP + TCP | N/A (module) |

All three have READMEs, working examples, and zero runtime dependencies.

## Ports

| Protocol | Port | Purpose |
|----------|------|---------|
| HTTP | 6790 | REST API, WebSocket, Dashboard, Blog, Examples |
| TCP | 6789 | High-throughput worker protocol |

## Routes (embedded server)

Public (no auth): `/`, `/blog/*`, `/examples`, `/api/v1/health`, `/api/v1/metrics/prometheus`, `/api/v1/openapi.json`, `/api/v1/docs`

Auth-protected: `/dashboard`, all job/queue/schedule/webhook endpoints.
