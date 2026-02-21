//! Configuration module for RustQueue.
//!
//! Provides typed configuration structs that map to `rustqueue.toml`.
//! All structs derive `Serialize`, `Deserialize`, `Debug`, `Clone`, and `PartialEq`,
//! and implement `Default` with sensible production-ready values.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Root configuration, corresponding to the full `rustqueue.toml` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RustQueueConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub webhooks: crate::engine::webhook::WebhookConfig,
    #[serde(default)]
    pub queues: QueueRateLimitMap,
}

// ---------------------------------------------------------------------------
// Sub-config structs
// ---------------------------------------------------------------------------

/// Network listener settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Bind address for both HTTP and TCP listeners.
    #[serde(default = "default_host")]
    pub host: String,
    /// Port for the HTTP/REST API.
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// Port for the binary TCP protocol.
    #[serde(default = "default_tcp_port")]
    pub tcp_port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            http_port: default_http_port(),
            tcp_port: default_tcp_port(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_http_port() -> u16 {
    6790
}
fn default_tcp_port() -> u16 {
    6789
}

// ---------------------------------------------------------------------------

/// Storage backend type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackendType {
    #[default]
    Redb,
    InMemory,
    Sqlite,
    Postgres,
    /// Hybrid memory+disk: in-memory DashMap hot path with periodic snapshot to redb.
    Hybrid,
}

/// Durability level for redb write commits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedbDurabilityConfig {
    /// Do not persist commits unless followed by a higher durability commit.
    ///
    /// Highest throughput, but unsafe for crash durability and can cause file growth
    /// if used exclusively for long periods.
    None,
    /// Persist on each commit before returning.
    #[default]
    Immediate,
    /// Queue persistence and return earlier for better throughput.
    Eventual,
}

/// High-level durability mode for the redb storage backend.
///
/// Controls the trade-off between write throughput and crash durability:
/// - `"immediate"`: fsync per write (~348 ops/sec). Zero data loss on crash.
/// - `"batched"`: coalesced writes (~22K ops/sec). Up to `write_coalescing_interval_ms`
///   of data loss on crash. **Default** for 60x throughput improvement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityMode {
    /// Fsync per write. Safest, but limited to ~348 ops/sec.
    Immediate,
    /// Buffer writes and flush periodically. ~22K ops/sec, with up to
    /// `write_coalescing_interval_ms` of data loss on crash.
    #[default]
    Batched,
}

/// Persistent storage settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Which storage engine to use.
    #[serde(default)]
    pub backend: StorageBackendType,
    /// Path to the data directory (for Redb / Sqlite).
    #[serde(default = "default_storage_path")]
    pub path: String,
    /// High-level durability mode: `"immediate"` (fsync per write) or `"batched"`
    /// (coalesced writes, 60x faster). Defaults to `"batched"`.
    ///
    /// When set to `"batched"`, write coalescing is automatically enabled.
    /// When set to `"immediate"`, write coalescing is disabled regardless of
    /// `write_coalescing_enabled`.
    #[serde(default)]
    pub durability: DurabilityMode,
    /// redb write durability mode (only used when `backend = "redb"`).
    #[serde(default)]
    pub redb_durability: RedbDurabilityConfig,
    /// Connection string for Postgres (only used when `backend = "postgres"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_url: Option<String>,
    /// Enable write coalescing for redb backend (buffers single writes into batches).
    /// Defaults to `true` (matching the `"batched"` durability mode).
    #[serde(default = "default_write_coalescing_enabled")]
    pub write_coalescing_enabled: bool,
    /// Flush interval for write coalescing in milliseconds.
    #[serde(default = "default_write_coalescing_interval_ms")]
    pub write_coalescing_interval_ms: u64,
    /// Maximum buffered writes before triggering a flush.
    #[serde(default = "default_write_coalescing_max_batch")]
    pub write_coalescing_max_batch: usize,
    /// Snapshot interval for hybrid storage in milliseconds.
    #[serde(default = "default_hybrid_snapshot_interval_ms")]
    pub hybrid_snapshot_interval_ms: u64,
    /// Maximum dirty entries before early flush in hybrid storage.
    #[serde(default = "default_hybrid_max_dirty")]
    pub hybrid_max_dirty: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackendType::default(),
            path: default_storage_path(),
            durability: DurabilityMode::default(),
            redb_durability: RedbDurabilityConfig::default(),
            postgres_url: None,
            write_coalescing_enabled: default_write_coalescing_enabled(),
            write_coalescing_interval_ms: default_write_coalescing_interval_ms(),
            write_coalescing_max_batch: default_write_coalescing_max_batch(),
            hybrid_snapshot_interval_ms: default_hybrid_snapshot_interval_ms(),
            hybrid_max_dirty: default_hybrid_max_dirty(),
        }
    }
}

fn default_write_coalescing_enabled() -> bool {
    true
}

fn default_storage_path() -> String {
    "./data".to_string()
}

fn default_write_coalescing_interval_ms() -> u64 {
    10
}

fn default_write_coalescing_max_batch() -> usize {
    100
}

fn default_hybrid_snapshot_interval_ms() -> u64 {
    1000
}

fn default_hybrid_max_dirty() -> usize {
    5000
}

// ---------------------------------------------------------------------------

/// Authentication / authorization settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Whether token-based auth is enforced.
    #[serde(default)]
    pub enabled: bool,
    /// List of valid bearer tokens.
    #[serde(default)]
    pub tokens: Vec<String>,
}

// ---------------------------------------------------------------------------

/// Internal scheduler tick and stall-detection settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Milliseconds between scheduler ticks (delayed-job promotion, cron evaluation).
    #[serde(default = "default_tick_interval_ms")]
    pub tick_interval_ms: u64,
    /// Milliseconds between stall-detection sweeps.
    #[serde(default = "default_stall_check_interval_ms")]
    pub stall_check_interval_ms: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick_interval_ms: default_tick_interval_ms(),
            stall_check_interval_ms: default_stall_check_interval_ms(),
        }
    }
}

fn default_tick_interval_ms() -> u64 {
    1000
}
fn default_stall_check_interval_ms() -> u64 {
    5000
}

// ---------------------------------------------------------------------------

/// Default job behaviour when the submitter does not specify overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsConfig {
    /// Maximum retry attempts.
    #[serde(default = "default_max_attempts")]
    pub default_max_attempts: u32,
    /// Backoff strategy name: "fixed", "linear", or "exponential".
    #[serde(default = "default_backoff")]
    pub default_backoff: String,
    /// Base delay between retries in milliseconds.
    #[serde(default = "default_backoff_delay_ms")]
    pub default_backoff_delay_ms: u64,
    /// Per-job processing timeout in milliseconds (5 minutes).
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: u64,
    /// How long a job can be active without a heartbeat before it is considered stalled.
    #[serde(default = "default_stall_timeout_ms")]
    pub stall_timeout_ms: u64,
    /// Maximum dependency chain depth for DAG flows (cycle detection).
    #[serde(default = "default_max_dag_depth")]
    pub max_dag_depth: usize,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            default_max_attempts: default_max_attempts(),
            default_backoff: default_backoff(),
            default_backoff_delay_ms: default_backoff_delay_ms(),
            default_timeout_ms: default_timeout_ms(),
            stall_timeout_ms: default_stall_timeout_ms(),
            max_dag_depth: default_max_dag_depth(),
        }
    }
}

fn default_max_attempts() -> u32 {
    3
}
fn default_backoff() -> String {
    "exponential".to_string()
}
fn default_backoff_delay_ms() -> u64 {
    1000
}
fn default_timeout_ms() -> u64 {
    300_000
}
fn default_stall_timeout_ms() -> u64 {
    30_000
}
fn default_max_dag_depth() -> usize {
    10
}

// ---------------------------------------------------------------------------

/// How long completed / failed / DLQ jobs are kept before automatic removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// TTL for completed jobs (human-readable, e.g. "7d").
    #[serde(default = "default_completed_ttl")]
    pub completed_ttl: String,
    /// TTL for failed jobs.
    #[serde(default = "default_failed_ttl")]
    pub failed_ttl: String,
    /// TTL for dead-letter-queue jobs.
    #[serde(default = "default_dlq_ttl")]
    pub dlq_ttl: String,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            completed_ttl: default_completed_ttl(),
            failed_ttl: default_failed_ttl(),
            dlq_ttl: default_dlq_ttl(),
        }
    }
}

fn default_completed_ttl() -> String {
    "7d".to_string()
}
fn default_failed_ttl() -> String {
    "30d".to_string()
}
fn default_dlq_ttl() -> String {
    "90d".to_string()
}

// ---------------------------------------------------------------------------

/// Built-in web dashboard settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Whether the dashboard is served.
    #[serde(default = "default_dashboard_enabled")]
    pub enabled: bool,
    /// URL path prefix for dashboard routes.
    #[serde(default = "default_dashboard_path_prefix")]
    pub path_prefix: String,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: default_dashboard_enabled(),
            path_prefix: default_dashboard_path_prefix(),
        }
    }
}

fn default_dashboard_enabled() -> bool {
    true
}
fn default_dashboard_path_prefix() -> String {
    "/dashboard".to_string()
}

// ---------------------------------------------------------------------------

/// Logging / tracing settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level filter: "trace", "debug", "info", "warn", "error".
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Output format: "pretty" (human-readable) or "json".
    #[serde(default = "default_log_format")]
    pub format: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_format() -> String {
    "pretty".to_string()
}

// ---------------------------------------------------------------------------

/// Observability / metrics settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Expose a Prometheus-compatible scrape endpoint.
    #[serde(default = "default_prometheus_enabled")]
    pub prometheus_enabled: bool,
    /// URL path for the Prometheus metrics endpoint.
    #[serde(default = "default_prometheus_path")]
    pub prometheus_path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            prometheus_enabled: default_prometheus_enabled(),
            prometheus_path: default_prometheus_path(),
        }
    }
}

fn default_prometheus_enabled() -> bool {
    true
}
fn default_prometheus_path() -> String {
    "/api/v1/metrics/prometheus".to_string()
}

// ---------------------------------------------------------------------------

/// TLS configuration for the TCP protocol listener.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Whether TLS is enabled for the TCP protocol.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded certificate chain file.
    #[serde(default)]
    pub cert_path: String,
    /// Path to the PEM-encoded private key file.
    #[serde(default)]
    pub key_path: String,
}

// ---------------------------------------------------------------------------
// Per-queue rate limiting
// ---------------------------------------------------------------------------

/// Per-queue rate limit configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueRateLimit {
    /// Maximum sustained push rate (jobs per second). None = unlimited.
    #[serde(default)]
    pub rate_limit_per_second: Option<f64>,
    /// Burst allowance above the sustained rate. Defaults to rate_limit_per_second.
    #[serde(default)]
    pub rate_limit_burst: Option<u32>,
}

/// Top-level map of queue names to their rate-limit configs.
///
/// Uses `#[serde(flatten)]` so that in TOML the queue entries appear directly
/// under `[queues]`, e.g. `[queues.emails]` rather than `[queues.queues.emails]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueRateLimitMap {
    #[serde(flatten)]
    pub queues: HashMap<String, QueueRateLimit>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = RustQueueConfig::default();

        // Server
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.http_port, 6790);
        assert_eq!(cfg.server.tcp_port, 6789);

        // Storage
        assert_eq!(cfg.storage.backend, StorageBackendType::Redb);
        assert_eq!(cfg.storage.path, "./data");
        assert_eq!(cfg.storage.durability, DurabilityMode::Batched);
        assert_eq!(cfg.storage.redb_durability, RedbDurabilityConfig::Immediate);
        assert!(cfg.storage.write_coalescing_enabled);
        assert_eq!(cfg.storage.postgres_url, None);

        // Auth
        assert!(!cfg.auth.enabled);
        assert!(cfg.auth.tokens.is_empty());

        // Scheduler
        assert_eq!(cfg.scheduler.tick_interval_ms, 1000);
        assert_eq!(cfg.scheduler.stall_check_interval_ms, 5000);

        // Jobs
        assert_eq!(cfg.jobs.default_max_attempts, 3);
        assert_eq!(cfg.jobs.default_backoff, "exponential");
        assert_eq!(cfg.jobs.default_backoff_delay_ms, 1000);
        assert_eq!(cfg.jobs.default_timeout_ms, 300_000);
        assert_eq!(cfg.jobs.stall_timeout_ms, 30_000);

        // Retention
        assert_eq!(cfg.retention.completed_ttl, "7d");
        assert_eq!(cfg.retention.failed_ttl, "30d");
        assert_eq!(cfg.retention.dlq_ttl, "90d");

        // Dashboard
        assert!(cfg.dashboard.enabled);
        assert_eq!(cfg.dashboard.path_prefix, "/dashboard");

        // Logging
        assert_eq!(cfg.logging.level, "info");
        assert_eq!(cfg.logging.format, "pretty");

        // Metrics
        assert!(cfg.metrics.prometheus_enabled);
        assert_eq!(cfg.metrics.prometheus_path, "/api/v1/metrics/prometheus");

        // TLS
        assert!(!cfg.tls.enabled);
        assert_eq!(cfg.tls.cert_path, "");
        assert_eq!(cfg.tls.key_path, "");
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let original = RustQueueConfig::default();
        let toml_str = toml::to_string(&original).expect("serialize to TOML");
        let parsed: RustQueueConfig = toml::from_str(&toml_str).expect("parse from TOML");
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_partial_toml_uses_defaults() {
        // Only specify a couple of fields; everything else should use defaults.
        let input = r#"
[server]
host = "127.0.0.1"

[storage]
backend = "postgres"
postgres_url = "postgres://localhost/rustqueue"
"#;
        let cfg: RustQueueConfig = toml::from_str(input).expect("parse partial TOML");

        // Overridden values
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.storage.backend, StorageBackendType::Postgres);
        assert_eq!(cfg.storage.redb_durability, RedbDurabilityConfig::Immediate);
        assert_eq!(
            cfg.storage.postgres_url.as_deref(),
            Some("postgres://localhost/rustqueue")
        );

        // Defaults should still apply for everything else
        assert_eq!(cfg.server.http_port, 6790);
        assert_eq!(cfg.server.tcp_port, 6789);
        assert_eq!(cfg.jobs.default_max_attempts, 3);
        assert!(cfg.dashboard.enabled);
    }

    #[test]
    fn test_tls_config_defaults() {
        let cfg = RustQueueConfig::default();
        assert!(!cfg.tls.enabled);
        assert_eq!(cfg.tls.cert_path, "");
        assert_eq!(cfg.tls.key_path, "");
    }

    #[test]
    fn test_tls_config_from_toml() {
        let input = r#"
[tls]
enabled = true
cert_path = "/etc/certs/server.crt"
key_path = "/etc/certs/server.key"
"#;
        let cfg: RustQueueConfig = toml::from_str(input).expect("parse TLS TOML");
        assert!(cfg.tls.enabled);
        assert_eq!(cfg.tls.cert_path, "/etc/certs/server.crt");
        assert_eq!(cfg.tls.key_path, "/etc/certs/server.key");
    }

    #[test]
    fn test_storage_backend_type_serde() {
        // Ensure snake_case serialization for the enum variants.
        let redb = StorageBackendType::Redb;
        let json = serde_json::to_string(&redb).unwrap();
        assert_eq!(json, "\"redb\"");

        let sqlite = StorageBackendType::Sqlite;
        let json = serde_json::to_string(&sqlite).unwrap();
        assert_eq!(json, "\"sqlite\"");

        let pg = StorageBackendType::Postgres;
        let json = serde_json::to_string(&pg).unwrap();
        assert_eq!(json, "\"postgres\"");

        let inmemory = StorageBackendType::InMemory;
        let json = serde_json::to_string(&inmemory).unwrap();
        assert_eq!(json, "\"in_memory\"");

        let hybrid = StorageBackendType::Hybrid;
        let json = serde_json::to_string(&hybrid).unwrap();
        assert_eq!(json, "\"hybrid\"");
    }

    #[test]
    fn test_redb_durability_serde() {
        let none = RedbDurabilityConfig::None;
        let json = serde_json::to_string(&none).unwrap();
        assert_eq!(json, "\"none\"");

        let immediate = RedbDurabilityConfig::Immediate;
        let json = serde_json::to_string(&immediate).unwrap();
        assert_eq!(json, "\"immediate\"");

        let eventual = RedbDurabilityConfig::Eventual;
        let json = serde_json::to_string(&eventual).unwrap();
        assert_eq!(json, "\"eventual\"");
    }

    #[test]
    fn test_queue_rate_limit_from_toml() {
        let input = r#"
[queues.emails]
rate_limit_per_second = 100.0
rate_limit_burst = 200

[queues.bulk]
rate_limit_per_second = 10.0

[queues.critical]
"#;
        let cfg: RustQueueConfig = toml::from_str(input).expect("parse queue rate limit TOML");
        assert_eq!(cfg.queues.queues.len(), 3);

        let emails = &cfg.queues.queues["emails"];
        assert_eq!(emails.rate_limit_per_second, Some(100.0));
        assert_eq!(emails.rate_limit_burst, Some(200));

        let bulk = &cfg.queues.queues["bulk"];
        assert_eq!(bulk.rate_limit_per_second, Some(10.0));
        assert_eq!(bulk.rate_limit_burst, None);

        let critical = &cfg.queues.queues["critical"];
        assert_eq!(critical.rate_limit_per_second, None);
        assert_eq!(critical.rate_limit_burst, None);
    }

    #[test]
    fn test_queue_rate_limit_defaults_empty() {
        let cfg = RustQueueConfig::default();
        assert!(cfg.queues.queues.is_empty());
    }

    #[test]
    fn test_durability_mode_serde() {
        let batched = DurabilityMode::Batched;
        let json = serde_json::to_string(&batched).unwrap();
        assert_eq!(json, "\"batched\"");

        let immediate = DurabilityMode::Immediate;
        let json = serde_json::to_string(&immediate).unwrap();
        assert_eq!(json, "\"immediate\"");
    }

    #[test]
    fn test_durability_mode_from_toml() {
        let input = r#"
[storage]
durability = "immediate"
"#;
        let cfg: RustQueueConfig = toml::from_str(input).expect("parse durability TOML");
        assert_eq!(cfg.storage.durability, DurabilityMode::Immediate);

        // Default is batched
        let cfg2 = RustQueueConfig::default();
        assert_eq!(cfg2.storage.durability, DurabilityMode::Batched);
    }
}
