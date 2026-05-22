# Changelog

All notable changes to RustQueue are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.3.0] — 2026-05-22

Production-embedded release. Embedded mode now delivers on the durability story,
and there's an ergonomic worker entry point.

### Fixed

- **Embedded housekeeping now runs.** Previously, `RustQueue::redb(...).build()`
  never started the background scheduler, so in embedded (library) mode:
  - backoff retries silently stalled (a failed job routed to `Delayed` was never
    promoted back),
  - crash recovery never fired (a job whose worker died stayed `Active` forever),
  - schedules never executed.
  These now work without running the server. Call `start_housekeeping()`, or use
  `run_worker` (which starts it automatically). This is a bug fix, not a breaking
  change — no public API was removed.

### Added

- **`run_worker(queue, handler)`** and **`run_worker_with_shutdown(queue, handler, shutdown)`** —
  a managed worker loop: pulls jobs, runs your handler, acks on `Ok`, fails (engine
  applies retry/backoff/DLQ) on `Err`, and auto-heartbeats the in-flight job so a
  healthy long-running job is never reclaimed by stall detection. Sequential
  (one job at a time). Stops on Ctrl-C, or when the supplied shutdown future resolves.
- **`start_housekeeping()`** — starts the background loop (delayed-job promotion,
  schedule execution, timeout + stall detection, retention). Idempotent; returns
  `Err` outside a Tokio runtime; the loop is aborted when the last `RustQueue`
  clone is dropped.
- **Builder knobs** `.stall_timeout(Duration)` (default 30s) and
  `.tick_interval(Duration)` (default 1s).
- **`RustQueue` is now `Clone`** (cheap `Arc` handle) — clone it into worker tasks.
- Embedded schedule API on `RustQueue`: `create_schedule`, `list_schedules`,
  `get_schedule`, `delete_schedule`, `pause_schedule`, `resume_schedule`, plus
  `heartbeat` and `get_dlq_jobs`.
- New example `examples/crash_recovery.rs` (survive `kill -9`, complete after restart)
  and a production-shaped `examples/axum_background_jobs.rs` (managed worker wired to
  graceful shutdown).
- Operational guide at `docs/production.md` and a website Production guide page.

## [0.2.0]

- Schedule engine, embedded web dashboard, DAG workflows, webhooks, and
  multi-backend storage (redb, memory, SQLite, PostgreSQL, hybrid). Published on
  crates.io.

[0.3.0]: https://github.com/ferax564/rustqueue/releases/tag/v0.3.0
