//! Prometheus metric name constants.
//!
//! Centralises all metric identifiers so they stay consistent between the
//! recording sites in [`super::queue::QueueManager`] and any dashboards or
//! alerting rules that reference them.

// ── Counters ─────────────────────────────────────────────────────────────────

/// Counter incremented each time a job is pushed (enqueued).
pub const JOBS_PUSHED_TOTAL: &str = "rustqueue_jobs_pushed_total";

/// Counter incremented each time a job is acknowledged (completed successfully).
pub const JOBS_COMPLETED_TOTAL: &str = "rustqueue_jobs_completed_total";

/// Counter incremented each time a job is reported as failed.
pub const JOBS_FAILED_TOTAL: &str = "rustqueue_jobs_failed_total";

/// Counter incremented by the number of jobs returned from a pull (dequeue).
pub const JOBS_PULLED_TOTAL: &str = "rustqueue_jobs_pulled_total";

/// Counter incremented each time a schedule fires.
pub const SCHEDULES_FIRED_TOTAL: &str = "rustqueue_schedules_fired_total";

// ── Gauges ───────────────────────────────────────────────────────────────────

/// Gauge tracking the number of waiting jobs per queue.
pub const QUEUE_WAITING_JOBS: &str = "rustqueue_queue_waiting_jobs";

/// Gauge tracking the number of active jobs per queue.
pub const QUEUE_ACTIVE_JOBS: &str = "rustqueue_queue_active_jobs";

/// Gauge tracking the number of delayed jobs per queue.
pub const QUEUE_DELAYED_JOBS: &str = "rustqueue_queue_delayed_jobs";

/// Gauge tracking the number of DLQ jobs per queue.
pub const QUEUE_DLQ_JOBS: &str = "rustqueue_queue_dlq_jobs";

/// Gauge tracking connected WebSocket clients.
pub const WEBSOCKET_CLIENTS_CONNECTED: &str = "rustqueue_websocket_clients_connected";

/// Gauge for the last scheduler tick duration in seconds.
pub const SCHEDULER_TICK_DURATION_SECONDS: &str = "rustqueue_scheduler_tick_duration_seconds";

// ── Histograms ───────────────────────────────────────────────────────────────

/// Histogram for push operation latency in seconds.
pub const PUSH_DURATION_SECONDS: &str = "rustqueue_push_duration_seconds";

/// Histogram for pull operation latency in seconds.
pub const PULL_DURATION_SECONDS: &str = "rustqueue_pull_duration_seconds";

/// Histogram for ack operation latency in seconds.
pub const ACK_DURATION_SECONDS: &str = "rustqueue_ack_duration_seconds";

// ── HTTP metrics ─────────────────────────────────────────────────────────────

/// Counter for total HTTP requests, labelled by method, path, and status class.
pub const HTTP_REQUESTS_TOTAL: &str = "rustqueue_http_requests_total";

/// Histogram for HTTP request duration in seconds.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "rustqueue_http_request_duration_seconds";

// ── Webhook metrics ─────────────────────────────────────────────────────────

/// Counter for successfully delivered webhook events.
pub const WEBHOOKS_DELIVERED_TOTAL: &str = "rustqueue_webhooks_delivered_total";

/// Counter for webhook delivery failures (all retries exhausted).
pub const WEBHOOKS_DELIVERY_FAILURES_TOTAL: &str = "rustqueue_webhooks_delivery_failures_total";

/// Counter for pushes rejected by per-queue rate limiting.
pub const RATE_LIMIT_REJECTED_TOTAL: &str = "rustqueue_rate_limit_rejected_total";
