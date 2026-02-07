/**
 * RustQueue Node.js SDK — TCP Client.
 *
 * Uses Node.js built-in `net.Socket` with newline-delimited JSON framing.
 * Supports automatic reconnection with exponential backoff.
 *
 * The TCP protocol provides lower overhead than HTTP for high-throughput
 * worker patterns (push/pull/ack loops).
 *
 * @example
 * ```ts
 * import { RustQueueTcpClient } from "@rustqueue/client";
 *
 * const tcp = new RustQueueTcpClient({ host: "127.0.0.1", port: 6789 });
 * await tcp.connect();
 *
 * const jobId = await tcp.push("emails", "send-welcome", { to: "alice@example.com" });
 * const jobs = await tcp.pull("emails");
 * await tcp.ack(jobs[0].id);
 *
 * tcp.disconnect();
 * ```
 *
 * @module
 */

import * as net from "node:net";
import { RustQueueError } from "./errors.js";
import type {
  AckBatchItem,
  AckBatchResponse,
  AckBatchResult,
  Job,
  JobOptions,
  PushJobInput,
  QueueCounts,
  Schedule,
  ScheduleInput,
  TcpClientOptions,
} from "./types.js";

/**
 * Pending request waiting for a response from the server.
 */
interface PendingRequest {
  resolve: (value: Record<string, unknown>) => void;
  reject: (reason: Error) => void;
}

/**
 * TCP client for the RustQueue wire protocol.
 *
 * The protocol is newline-delimited JSON: each command is a single JSON
 * object terminated by `\n`, and each response is a single JSON object
 * terminated by `\n`. Commands are processed sequentially (one at a time).
 *
 * All methods throw {@link RustQueueError} on failure.
 */
export class RustQueueTcpClient {
  private readonly host: string;
  private readonly port: number;
  private readonly token?: string;
  private readonly autoReconnect: boolean;
  private readonly maxReconnectAttempts: number;
  private readonly reconnectDelayMs: number;

  private socket: net.Socket | null = null;
  private connected = false;
  private connecting = false;
  private intentionalClose = false;
  private reconnectAttempts = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  /** Buffer for accumulating partial line data from the socket. */
  private buffer = "";

  /** FIFO queue of pending requests awaiting server responses. */
  private pending: PendingRequest[] = [];

  /**
   * Create a new TCP client.
   *
   * @param options - Connection options including host, port, and optional auth token.
   */
  constructor(options: TcpClientOptions) {
    this.host = options.host;
    this.port = options.port;
    this.token = options.token;
    this.autoReconnect = options.autoReconnect ?? true;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 10;
    this.reconnectDelayMs = options.reconnectDelayMs ?? 1000;
  }

  // ── Connection Management ────────────────────────────────────────────────

  /**
   * Open a TCP connection to the RustQueue server.
   *
   * If a token was provided, sends an `auth` command immediately after
   * the TCP connection is established. Throws if authentication fails.
   */
  async connect(): Promise<void> {
    if (this.connected) {
      return;
    }
    if (this.connecting) {
      throw new RustQueueError(
        "CONNECTION_ERROR",
        "Connection attempt already in progress"
      );
    }

    this.intentionalClose = false;
    await this.establishConnection();
  }

  /**
   * Close the TCP connection. Rejects all pending requests.
   *
   * Disables automatic reconnection. Call {@link connect} to re-establish.
   */
  disconnect(): void {
    this.intentionalClose = true;
    this.clearReconnectTimer();
    this.cleanup("Client disconnected");
  }

  /** Whether the client is currently connected. */
  get isConnected(): boolean {
    return this.connected;
  }

  // ── Job Operations ───────────────────────────────────────────────────────

  /**
   * Push a single job to a queue.
   *
   * @param queue   - Target queue name.
   * @param name    - Job name (type identifier).
   * @param data    - Arbitrary JSON payload.
   * @param options - Optional job configuration.
   * @returns The UUID of the created job.
   */
  async push(
    queue: string,
    name: string,
    data: unknown,
    options?: JobOptions
  ): Promise<string> {
    const cmd: Record<string, unknown> = { cmd: "push", queue, name, data };
    if (options) {
      cmd.options = options;
    }
    const resp = await this.send(cmd);
    return resp.id as string;
  }

  /**
   * Push multiple jobs to a queue in a single TCP command.
   *
   * @param queue - Target queue name.
   * @param jobs  - Array of job definitions.
   * @returns Array of UUIDs for the created jobs.
   */
  async pushBatch(queue: string, jobs: PushJobInput[]): Promise<string[]> {
    const batchJobs = jobs.map((job) => {
      const entry: Record<string, unknown> = { name: job.name, data: job.data };
      if (job.options) {
        entry.options = job.options;
      }
      return entry;
    });
    const resp = await this.send({
      cmd: "push_batch",
      queue,
      jobs: batchJobs,
    });
    return resp.ids as string[];
  }

  /**
   * Pull one or more jobs from a queue.
   *
   * @param queue - Queue to pull from.
   * @param count - Number of jobs to pull. Defaults to 1.
   * @returns Array of active jobs (may be empty).
   */
  async pull(queue: string, count?: number): Promise<Job[]> {
    const cmd: Record<string, unknown> = { cmd: "pull", queue };
    if (count !== undefined) {
      cmd.count = count;
    }
    const resp = await this.send(cmd);

    // Server returns { job: Job|null } for count=1, { jobs: Job[] } for count>1.
    if (resp.jobs) {
      return resp.jobs as Job[];
    }
    if (resp.job && resp.job !== null) {
      return [resp.job as Job];
    }
    return [];
  }

  /**
   * Acknowledge successful completion of a job.
   *
   * @param jobId  - UUID of the job.
   * @param result - Optional result data.
   */
  async ack(jobId: string, result?: unknown): Promise<void> {
    const cmd: Record<string, unknown> = { cmd: "ack", id: jobId };
    if (result !== undefined) {
      cmd.result = result;
    }
    await this.send(cmd);
  }

  /**
   * Acknowledge multiple jobs in a single TCP command.
   *
   * @param items - Array of ack items, each with an `id` and optional `result`.
   * @returns Batch ack response with per-item results.
   */
  async ackBatch(items: AckBatchItem[]): Promise<AckBatchResponse> {
    const batchItems = items.map((item) => {
      const entry: Record<string, unknown> = { id: item.id };
      if (item.result !== undefined) {
        entry.result = item.result;
      }
      return entry;
    });
    const resp = await this.send({ cmd: "ack_batch", items: batchItems });
    return {
      ok: resp.ok as boolean,
      acked: resp.acked as number,
      failed: resp.failed as number,
      results: resp.results as AckBatchResult[],
    };
  }

  /**
   * Report that a job has failed.
   *
   * @param jobId - UUID of the failed job.
   * @param error - Human-readable error message.
   * @returns Whether the job will be retried and when.
   */
  async fail(
    jobId: string,
    error: string
  ): Promise<{ will_retry: boolean; next_attempt_at: string | null }> {
    const resp = await this.send({ cmd: "fail", id: jobId, error });
    return {
      will_retry: resp.will_retry as boolean,
      next_attempt_at: (resp.next_attempt_at as string | null) ?? null,
    };
  }

  /**
   * Cancel a job.
   *
   * @param jobId - UUID of the job to cancel.
   */
  async cancel(jobId: string): Promise<void> {
    await this.send({ cmd: "cancel", id: jobId });
  }

  /**
   * Update the progress of an active job.
   *
   * @param jobId    - UUID of the active job.
   * @param progress - Progress value from 0 to 100.
   * @param message  - Optional human-readable progress message.
   */
  async progress(
    jobId: string,
    progress: number,
    message?: string
  ): Promise<void> {
    const cmd: Record<string, unknown> = {
      cmd: "progress",
      id: jobId,
      progress,
    };
    if (message !== undefined) {
      cmd.message = message;
    }
    await this.send(cmd);
  }

  /**
   * Send a heartbeat for an active job.
   *
   * @param jobId - UUID of the active job.
   */
  async heartbeat(jobId: string): Promise<void> {
    await this.send({ cmd: "heartbeat", id: jobId });
  }

  /**
   * Get full details for a specific job.
   *
   * Note: The TCP protocol does not have a dedicated `get` command in the
   * current server implementation. This uses the `stats` command pattern.
   * If the server adds a `get` command, this will use it.
   *
   * @param jobId - UUID of the job.
   * @returns The job object, or `null` if not found.
   */
  async getJob(jobId: string): Promise<Job | null> {
    try {
      const resp = await this.send({ cmd: "get", id: jobId });
      return (resp.job as Job) ?? null;
    } catch (err) {
      if (err instanceof RustQueueError && err.code === "NOT_FOUND") {
        return null;
      }
      throw err;
    }
  }

  // ── Queue Operations ─────────────────────────────────────────────────────

  /**
   * Get statistics for a specific queue.
   *
   * @param queue - Queue name.
   * @returns Counts for each job state.
   */
  async getQueueStats(queue: string): Promise<QueueCounts> {
    const resp = await this.send({ cmd: "stats", queue });
    return resp.counts as QueueCounts;
  }

  // ── Schedule Operations ──────────────────────────────────────────────────

  /**
   * Create or update a schedule.
   *
   * @param schedule - Schedule definition.
   */
  async createSchedule(schedule: ScheduleInput): Promise<void> {
    await this.send({
      cmd: "schedule_create",
      name: schedule.name,
      queue: schedule.queue,
      job_name: schedule.job_name,
      job_data: schedule.job_data ?? {},
      job_options: schedule.job_options,
      cron_expr: schedule.cron_expr,
      every_ms: schedule.every_ms,
      timezone: schedule.timezone,
      max_executions: schedule.max_executions,
    });
  }

  /**
   * List all schedules.
   *
   * @returns Array of schedule objects.
   */
  async listSchedules(): Promise<Schedule[]> {
    const resp = await this.send({ cmd: "schedule_list" });
    return resp.schedules as Schedule[];
  }

  /**
   * Get a specific schedule by name.
   *
   * @param name - Schedule name.
   * @returns The schedule object, or `null` if not found.
   */
  async getSchedule(name: string): Promise<Schedule | null> {
    try {
      const resp = await this.send({ cmd: "schedule_get", name });
      return (resp.schedule as Schedule) ?? null;
    } catch (err) {
      if (err instanceof RustQueueError && err.code === "NOT_FOUND") {
        return null;
      }
      throw err;
    }
  }

  /**
   * Delete a schedule.
   *
   * @param name - Schedule name.
   */
  async deleteSchedule(name: string): Promise<void> {
    await this.send({ cmd: "schedule_delete", name });
  }

  /**
   * Pause a schedule.
   *
   * @param name - Schedule name.
   */
  async pauseSchedule(name: string): Promise<void> {
    await this.send({ cmd: "schedule_pause", name });
  }

  /**
   * Resume a paused schedule.
   *
   * @param name - Schedule name.
   */
  async resumeSchedule(name: string): Promise<void> {
    await this.send({ cmd: "schedule_resume", name });
  }

  // ── Internal: Connection ─────────────────────────────────────────────────

  /**
   * Establish the raw TCP connection and optionally authenticate.
   */
  private async establishConnection(): Promise<void> {
    this.connecting = true;

    try {
      await new Promise<void>((resolve, reject) => {
        const socket = new net.Socket();
        this.socket = socket;

        const onError = (...args: unknown[]) => {
          const err = args[0] as Error;
          socket.removeListener("error", onError);
          reject(
            new RustQueueError(
              "CONNECTION_ERROR",
              `Failed to connect to ${this.host}:${this.port}: ${err.message}`
            )
          );
        };

        socket.once("error", onError);

        socket.connect(this.port, this.host, () => {
          socket.removeListener("error", onError);
          this.connected = true;
          this.reconnectAttempts = 0;
          this.buffer = "";
          this.setupSocketHandlers(socket);
          resolve();
        });
      });

      // Authenticate if a token is configured.
      if (this.token) {
        await this.authenticate();
      }
    } finally {
      this.connecting = false;
    }
  }

  /**
   * Set up data, error, and close handlers on the connected socket.
   */
  private setupSocketHandlers(socket: net.Socket): void {
    socket.setEncoding("utf-8");

    socket.on("data", (chunk: string) => {
      this.buffer += chunk;
      this.processBuffer();
    });

    socket.on("error", (err: Error) => {
      this.cleanup(`Socket error: ${err.message}`);
      this.maybeReconnect();
    });

    socket.on("close", () => {
      if (!this.intentionalClose) {
        this.cleanup("Connection closed by server");
        this.maybeReconnect();
      }
    });
  }

  /**
   * Process accumulated buffer data, extracting complete newline-delimited
   * JSON responses and resolving the corresponding pending requests.
   */
  private processBuffer(): void {
    let newlineIdx: number;
    while ((newlineIdx = this.buffer.indexOf("\n")) !== -1) {
      const line = this.buffer.slice(0, newlineIdx);
      this.buffer = this.buffer.slice(newlineIdx + 1);

      if (line.trim().length === 0) {
        continue;
      }

      const pending = this.pending.shift();
      if (!pending) {
        // Unexpected response with no pending request — ignore.
        continue;
      }

      try {
        const json = JSON.parse(line) as Record<string, unknown>;
        if (json.ok === false) {
          const error = json.error as
            | { code: string; message: string }
            | undefined;
          if (error) {
            pending.reject(RustQueueError.fromServerError(error));
          } else {
            pending.reject(
              new RustQueueError(
                "UNKNOWN_ERROR",
                "Server returned ok=false without error details"
              )
            );
          }
        } else {
          pending.resolve(json);
        }
      } catch {
        pending.reject(
          new RustQueueError("PARSE_ERROR", `Invalid JSON response: ${line}`)
        );
      }
    }
  }

  /**
   * Send the authentication command and wait for the response.
   */
  private async authenticate(): Promise<void> {
    const resp = await this.send({ cmd: "auth", token: this.token });
    if (resp.ok !== true) {
      throw new RustQueueError("UNAUTHORIZED", "Authentication failed");
    }
  }

  /**
   * Send a JSON command over the TCP connection and wait for the response.
   *
   * Commands are serialized as a single line of JSON followed by `\n`.
   * Responses are matched in FIFO order.
   */
  private send(command: Record<string, unknown>): Promise<Record<string, unknown>> {
    if (!this.connected || !this.socket) {
      return Promise.reject(
        new RustQueueError(
          "CONNECTION_ERROR",
          "Not connected. Call connect() first."
        )
      );
    }

    return new Promise<Record<string, unknown>>((resolve, reject) => {
      this.pending.push({ resolve, reject });

      const line = JSON.stringify(command) + "\n";
      this.socket!.write(line, "utf-8", (err?: Error) => {
        if (err) {
          // Remove the pending request we just added since the write failed.
          const idx = this.pending.findIndex((p) => p.resolve === resolve);
          if (idx !== -1) {
            this.pending.splice(idx, 1);
          }
          reject(
            new RustQueueError(
              "WRITE_ERROR",
              `Failed to write to socket: ${err.message}`
            )
          );
        }
      });
    });
  }

  /**
   * Clean up connection state and reject all pending requests.
   */
  private cleanup(reason: string): void {
    this.connected = false;

    if (this.socket) {
      this.socket.removeAllListeners();
      this.socket.destroy();
      this.socket = null;
    }

    // Reject all pending requests.
    const pendingCopy = this.pending.splice(0);
    for (const req of pendingCopy) {
      req.reject(new RustQueueError("CONNECTION_LOST", reason));
    }

    this.buffer = "";
  }

  /**
   * Attempt reconnection with exponential backoff, if configured.
   */
  private maybeReconnect(): void {
    if (this.intentionalClose || !this.autoReconnect) {
      return;
    }
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      return;
    }

    const delay =
      this.reconnectDelayMs * Math.pow(2, this.reconnectAttempts);
    this.reconnectAttempts++;

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.establishConnection().catch(() => {
        // Reconnection failed — maybeReconnect will be called again
        // from the socket error/close handler.
      });
    }, delay);
  }

  /**
   * Cancel any pending reconnection timer.
   */
  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }
}
