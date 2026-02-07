/**
 * RustQueue Node.js SDK — Type definitions.
 *
 * These types mirror the Rust server's data model exactly.
 * Field names use snake_case to match the JSON wire format.
 * @module
 */

// ── Job State ────────────────────────────────────────────────────────────────

/**
 * All possible states in a job's lifecycle.
 *
 * Matches the Rust `JobState` enum (serialized as snake_case strings).
 */
export type JobState =
  | "created"
  | "waiting"
  | "delayed"
  | "active"
  | "completed"
  | "failed"
  | "dlq"
  | "cancelled"
  | "blocked";

/**
 * Strategy for increasing delay between retry attempts.
 *
 * - `fixed`       — same delay every retry
 * - `linear`      — delay * attempt number
 * - `exponential` — delay * 2^attempt (default)
 */
export type BackoffStrategy = "fixed" | "linear" | "exponential";

// ── Job ──────────────────────────────────────────────────────────────────────

/** A single log entry appended by a worker during job processing. */
export interface LogEntry {
  timestamp: string;
  message: string;
}

/**
 * A unit of work submitted to a queue for processing by a worker.
 *
 * This is the full job object as returned by the server.
 */
export interface Job {
  // Identity
  id: string;
  custom_id: string | null;
  name: string;
  queue: string;

  // Payload
  data: unknown;
  result: unknown | null;

  // State
  state: JobState;
  progress: number | null;
  logs: LogEntry[];

  // Scheduling
  priority: number;
  delay_until: string | null;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  completed_at: string | null;

  // Retry configuration
  max_attempts: number;
  attempt: number;
  backoff: BackoffStrategy;
  backoff_delay_ms: number;
  last_error: string | null;

  // Constraints
  ttl_ms: number | null;
  timeout_ms: number | null;
  unique_key: string | null;

  // Organization
  tags: string[];
  group_id: string | null;

  // Dependencies
  depends_on: string[];
  flow_id: string | null;

  // Behavior flags
  lifo: boolean;
  remove_on_complete: boolean;
  remove_on_fail: boolean;

  // Worker assignment
  worker_id: string | null;
  last_heartbeat: string | null;
}

// ── Job Options ──────────────────────────────────────────────────────────────

/**
 * Optional parameters when pushing a job.
 *
 * All fields are optional; the server applies sensible defaults for any
 * omitted values (e.g. priority=0, max_attempts=3, backoff=exponential).
 */
export interface JobOptions {
  /** Job priority. Higher numbers are dequeued first in priority mode. */
  priority?: number;
  /** Delay in milliseconds before the job becomes eligible for pulling. */
  delay_ms?: number;
  /** Maximum number of attempts (including the first). */
  max_attempts?: number;
  /** Backoff strategy for retries. */
  backoff?: BackoffStrategy;
  /** Base delay in milliseconds for backoff calculation. */
  backoff_delay_ms?: number;
  /** Time-to-live in milliseconds. Job expires if not started within this window. */
  ttl_ms?: number;
  /** Timeout in milliseconds. Active job is failed if it exceeds this duration. */
  timeout_ms?: number;
  /** Unique key for deduplication. Only one non-terminal job with a given key can exist. */
  unique_key?: string;
  /** Arbitrary tags for filtering and organization. */
  tags?: string[];
  /** Group ID for fair-scheduling across groups. */
  group_id?: string;
  /** If true, job is added to the front of the queue (LIFO). */
  lifo?: boolean;
  /** If true, job data is deleted from storage on completion. */
  remove_on_complete?: boolean;
  /** If true, job data is deleted from storage on final failure. */
  remove_on_fail?: boolean;
  /** Custom ID for external correlation. */
  custom_id?: string;
}

/**
 * Input for pushing a single job in a batch operation.
 */
export interface PushJobInput {
  /** Job name (type identifier). */
  name: string;
  /** Arbitrary JSON payload for the worker. */
  data: unknown;
  /** Optional job configuration. */
  options?: JobOptions;
}

// ── Ack Batch ────────────────────────────────────────────────────────────────

/** A single item in a batch ack request. */
export interface AckBatchItem {
  /** Job ID to acknowledge. */
  id: string;
  /** Optional result data to store with the completed job. */
  result?: unknown;
}

/** Result for a single item in a batch ack response. */
export interface AckBatchResult {
  id: string;
  ok: boolean;
  error?: {
    code: string;
    message: string;
  };
}

/** Full response from a batch ack operation. */
export interface AckBatchResponse {
  ok: boolean;
  acked: number;
  failed: number;
  results: AckBatchResult[];
}

// ── Queue ────────────────────────────────────────────────────────────────────

/** Summary counts for a queue, broken down by job state. */
export interface QueueCounts {
  waiting: number;
  active: number;
  delayed: number;
  completed: number;
  failed: number;
  dlq: number;
}

/** High-level information about a single queue. */
export interface QueueInfo {
  name: string;
  counts: QueueCounts;
}

// ── Schedule ─────────────────────────────────────────────────────────────────

/**
 * Input for creating or updating a schedule.
 *
 * A schedule automatically creates jobs at defined intervals.
 * Either `cron_expr` or `every_ms` (or both) must be provided.
 */
export interface ScheduleInput {
  /** Unique schedule name (used as identifier for get/delete/pause/resume). */
  name: string;
  /** Target queue for generated jobs. */
  queue: string;
  /** Job name (type identifier) for generated jobs. */
  job_name: string;
  /** JSON payload for generated jobs. */
  job_data?: unknown;
  /** Optional job configuration applied to every generated job. */
  job_options?: JobOptions;
  /** Cron expression (e.g. "0 * * * *" for every hour). */
  cron_expr?: string;
  /** Interval in milliseconds between runs. */
  every_ms?: number;
  /** IANA timezone for cron evaluation (e.g. "America/New_York"). */
  timezone?: string;
  /** Stop after this many executions. */
  max_executions?: number;
}

/** Full schedule object as returned by the server. */
export interface Schedule {
  name: string;
  queue: string;
  job_name: string;
  job_data: unknown;
  job_options: JobOptions | null;

  cron_expr: string | null;
  every_ms: number | null;
  timezone: string | null;

  max_executions: number | null;
  execution_count: number;
  paused: boolean;

  last_run_at: string | null;
  next_run_at: string | null;
  created_at: string;
  updated_at: string;
}

// ── Health ────────────────────────────────────────────────────────────────────

/** Response from the health check endpoint. */
export interface HealthResponse {
  ok: boolean;
  status: string;
  version: string;
  uptime_seconds: number;
}

// ── Error ────────────────────────────────────────────────────────────────────

/** Structured error returned by the RustQueue server. */
export interface RustQueueErrorInfo {
  code: string;
  message: string;
}

// ── Client Options ───────────────────────────────────────────────────────────

/** Configuration for the HTTP client. */
export interface HttpClientOptions {
  /** Base URL of the RustQueue HTTP server (e.g. "http://localhost:6790"). */
  baseUrl: string;
  /** Optional bearer token for authentication. */
  token?: string;
}

/** Configuration for the TCP client. */
export interface TcpClientOptions {
  /** Hostname or IP address of the RustQueue TCP server. */
  host: string;
  /** TCP port number (default: 6789). */
  port: number;
  /** Optional bearer token for authentication. */
  token?: string;
  /** Whether to automatically reconnect on disconnect. Default: true. */
  autoReconnect?: boolean;
  /** Maximum number of reconnection attempts. Default: 10. */
  maxReconnectAttempts?: number;
  /** Base delay in milliseconds between reconnection attempts (exponential backoff). Default: 1000. */
  reconnectDelayMs?: number;
}
