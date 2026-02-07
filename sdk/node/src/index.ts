/**
 * @rustqueue/client — Official Node.js SDK for RustQueue.
 *
 * Provides both HTTP and TCP clients for interacting with the RustQueue
 * distributed job scheduler. Zero external runtime dependencies.
 *
 * ## Quick Start
 *
 * ```ts
 * import { RustQueueClient, RustQueueTcpClient } from "@rustqueue/client";
 *
 * // HTTP client (simpler, good for most use cases)
 * const http = new RustQueueClient({ baseUrl: "http://localhost:6790" });
 * const jobId = await http.push("emails", "send-welcome", { to: "alice@example.com" });
 *
 * // TCP client (lower overhead, for high-throughput workers)
 * const tcp = new RustQueueTcpClient({ host: "127.0.0.1", port: 6789 });
 * await tcp.connect();
 * const jobs = await tcp.pull("emails");
 * ```
 *
 * @module
 */

// Client classes
export { RustQueueClient } from "./http-client.js";
export { RustQueueTcpClient } from "./tcp-client.js";

// Error class
export { RustQueueError } from "./errors.js";

// All types
export type {
  // Core types
  Job,
  JobState,
  JobOptions,
  BackoffStrategy,
  LogEntry,
  PushJobInput,

  // Ack batch
  AckBatchItem,
  AckBatchResult,
  AckBatchResponse,

  // Queue types
  QueueInfo,
  QueueCounts,

  // Schedule types
  Schedule,
  ScheduleInput,

  // Health
  HealthResponse,

  // Error info
  RustQueueErrorInfo,

  // Client options
  HttpClientOptions,
  TcpClientOptions,
} from "./types.js";
