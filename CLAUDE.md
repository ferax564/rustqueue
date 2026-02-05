# RustQueue — Claude Code Project Guide

## Project Overview

RustQueue is a high-performance distributed job scheduler written in Rust. Zero external dependencies, single-binary deployment. See `PRD.md` for full product requirements.

## Quick Commands

```bash
cargo check              # Type-check without building
cargo test               # Run all tests
cargo test -- --nocapture # Run tests with stdout visible
cargo build              # Debug build
cargo build --release    # Release build (optimized, stripped)
cargo bench              # Run benchmarks (criterion)
cargo clippy             # Lint
cargo fmt                # Format code
```

## Architecture

```
src/
├── main.rs          # Binary entry point: CLI parsing, server startup
├── lib.rs           # Library root: re-exports core types for embedded use
├── api/             # HTTP REST API (axum routes, handlers, middleware)
├── engine/          # Core business logic
│   ├── models.rs    # Data types: Job, Schedule, Worker, enums
│   ├── queue.rs     # Queue manager: insert, dequeue, state transitions
│   ├── scheduler.rs # Cron/interval scheduler: tick loop, job creation
│   ├── worker.rs    # Worker manager: registration, heartbeat, stall detection
│   ├── flow.rs      # DAG flow engine: dependency resolution, cycle detection
│   ├── dlq.rs       # Dead letter queue manager
│   └── rate.rs      # Token bucket rate limiter
├── storage/         # Storage abstraction and backends
│   ├── mod.rs       # StorageBackend trait definition
│   ├── redb.rs      # redb embedded backend (default, P0)
│   ├── sqlite.rs    # SQLite backend (P1)
│   └── postgres.rs  # PostgreSQL backend (P2)
├── protocol/        # TCP protocol: newline-delimited JSON
├── config/          # Configuration loading: TOML + env + CLI
└── dashboard/       # Embedded web dashboard (rust-embed)
```

## Key Design Decisions

- **Storage trait**: All backends implement `StorageBackend` (see `src/storage/mod.rs`). This allows swapping redb/sqlite/postgres without changing engine code.
- **Dual protocol**: HTTP (port 6790) for general use + TCP (port 6789) for high-throughput workers. Both support identical operations.
- **Crash-only design**: All state is persisted before acknowledging writes. Safe to `kill -9` at any time.
- **UUID v7 for job IDs**: Time-sortable, globally unique, no coordination needed.

## Conventions

- **Error handling**: Use `thiserror` for library errors, `anyhow` for binary/integration code. Storage trait methods return `anyhow::Result`.
- **Async**: Everything async via tokio. Use `#[async_trait]` for trait objects.
- **Serialization**: All public types derive `Serialize, Deserialize` via serde. JSON payloads use `serde_json::Value`.
- **Testing**: Unit tests in `#[cfg(test)]` modules within source files. Integration tests in `tests/`. Property-based tests with `proptest`. Benchmarks with `criterion` in `benches/`.
- **Naming**: snake_case for files/functions, PascalCase for types, SCREAMING_CASE for constants. Module names match their domain concept.

## Testing Strategy

- **Unit tests**: Each module has `#[cfg(test)] mod tests` testing individual functions.
- **Integration tests**: `tests/` directory tests full server behavior (HTTP + TCP).
- **Property tests**: State machine transitions, serialization roundtrips.
- **Benchmarks**: `benches/throughput.rs` measures jobs/sec for push, pull, ack operations.

## Configuration Priority

CLI flags > Environment variables (`RUSTQUEUE_*`) > Config file (`rustqueue.toml`) > Defaults

## Ports

| Protocol | Default Port | Purpose |
|----------|-------------|---------|
| HTTP     | 6790        | REST API, WebSocket, Dashboard |
| TCP      | 6789        | High-throughput worker protocol |
| Raft     | 6800        | Cluster consensus (when enabled) |

## Dependencies of Note

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP framework + WebSocket |
| `tokio` | Async runtime |
| `redb` | Embedded ACID storage (default backend) |
| `croner` | Cron expression parsing |
| `openraft` | Raft consensus (future, cluster mode) |
| `metrics` + `metrics-exporter-prometheus` | Prometheus metrics |
| `rust-embed` | Compile dashboard assets into binary |
