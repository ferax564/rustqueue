/**
 * RustQueue Node.js SDK — HTTP Client.
 *
 * Uses the built-in `fetch` API (Node.js 18+). Zero external dependencies.
 *
 * @example
 * ```ts
 * import { RustQueueClient } from "@rustqueue/client";
 *
 * const client = new RustQueueClient({ baseUrl: "http://localhost:6790" });
 *
 * const jobId = await client.push("emails", "send-welcome", { to: "alice@example.com" });
 * const jobs = await client.pull("emails");
 * await client.ack(jobs[0].id, { sent: true });
 * ```
 *
 * @module
 */

import { RustQueueError } from "./errors.js";
import type {
  HealthResponse,
  HttpClientOptions,
  Job,
  JobOptions,
  PushJobInput,
  QueueCounts,
  QueueInfo,
  Schedule,
  ScheduleInput,
} from "./types.js";

/**
 * HTTP client for the RustQueue REST API.
 *
 * All methods throw {@link RustQueueError} on failure. The error includes
 * the server's error code and message when available.
 */
export class RustQueueClient {
  private readonly baseUrl: string;
  private readonly headers: Record<string, string>;

  /**
   * Create a new HTTP client.
   *
   * @param options - Connection options including base URL and optional auth token.
   */
  constructor(options: HttpClientOptions) {
    // Strip trailing slash for consistent URL construction.
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.headers = {
      "Content-Type": "application/json",
      Accept: "application/json",
    };
    if (options.token) {
      this.headers["Authorization"] = `Bearer ${options.token}`;
    }
  }

  // ── Job Operations ───────────────────────────────────────────────────────

  /**
   * Push a single job to a queue.
   *
   * @param queue   - Target queue name.
   * @param name    - Job name (type identifier used by workers to dispatch logic).
   * @param data    - Arbitrary JSON payload for the worker.
   * @param options - Optional job configuration (priority, retries, delay, etc.).
   * @returns The UUID of the created job.
   *
   * @example
   * ```ts
   * const id = await client.push("emails", "send-welcome", {
   *   to: "alice@example.com",
   *   template: "welcome",
   * }, { priority: 10, max_attempts: 5 });
   * ```
   */
  async push(
    queue: string,
    name: string,
    data: unknown,
    options?: JobOptions
  ): Promise<string> {
    const body: Record<string, unknown> = { name, data };
    // Flatten options into the body, matching the server's #[serde(flatten)] behavior.
    if (options) {
      Object.assign(body, options);
    }
    const resp = await this.request<{ id: string }>(
      "POST",
      `/api/v1/queues/${encodeURIComponent(queue)}/jobs`,
      body
    );
    return resp.id;
  }

  /**
   * Push multiple jobs to a queue in a single request.
   *
   * @param queue - Target queue name.
   * @param jobs  - Array of job definitions.
   * @returns Array of UUIDs for the created jobs, in the same order as the input.
   *
   * @example
   * ```ts
   * const ids = await client.pushBatch("emails", [
   *   { name: "send-welcome", data: { to: "alice@example.com" } },
   *   { name: "send-welcome", data: { to: "bob@example.com" }, options: { priority: 5 } },
   * ]);
   * ```
   */
  async pushBatch(queue: string, jobs: PushJobInput[]): Promise<string[]> {
    const batchBody = jobs.map((job) => {
      const entry: Record<string, unknown> = { name: job.name, data: job.data };
      if (job.options) {
        Object.assign(entry, job.options);
      }
      return entry;
    });
    const resp = await this.request<{ ids: string[] }>(
      "POST",
      `/api/v1/queues/${encodeURIComponent(queue)}/jobs`,
      batchBody
    );
    return resp.ids;
  }

  /**
   * Pull one or more jobs from a queue.
   *
   * Pulled jobs transition to `active` state. The worker must eventually
   * call {@link ack} or {@link fail} for each pulled job.
   *
   * @param queue - Queue to pull from.
   * @param count - Number of jobs to pull. Defaults to 1.
   * @returns Array of active jobs (may be empty if the queue has no waiting jobs).
   */
  async pull(queue: string, count?: number): Promise<Job[]> {
    const query = count && count > 1 ? `?count=${count}` : "";
    const resp = await this.request<{ job?: Job | null; jobs?: Job[] }>(
      "GET",
      `/api/v1/queues/${encodeURIComponent(queue)}/jobs${query}`
    );

    // The server returns { job: Job|null } for count=1, { jobs: Job[] } for count>1.
    if (resp.jobs) {
      return resp.jobs;
    }
    if (resp.job) {
      return [resp.job];
    }
    return [];
  }

  /**
   * Acknowledge successful completion of a job.
   *
   * @param jobId  - UUID of the job to acknowledge.
   * @param result - Optional result data to store with the completed job.
   */
  async ack(jobId: string, result?: unknown): Promise<void> {
    const body = result !== undefined ? { result } : undefined;
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/jobs/${encodeURIComponent(jobId)}/ack`,
      body
    );
  }

  /**
   * Report that a job has failed.
   *
   * The server will determine whether to retry (based on max_attempts and backoff)
   * or move the job to the dead-letter queue.
   *
   * @param jobId - UUID of the failed job.
   * @param error - Human-readable error message.
   */
  async fail(
    jobId: string,
    error: string
  ): Promise<{ retry: boolean; next_attempt_at: string | null }> {
    const resp = await this.request<{
      retry: boolean;
      next_attempt_at: string | null;
    }>("POST", `/api/v1/jobs/${encodeURIComponent(jobId)}/fail`, { error });
    return { retry: resp.retry, next_attempt_at: resp.next_attempt_at ?? null };
  }

  /**
   * Cancel a job. Only non-active, non-completed jobs can be cancelled.
   *
   * @param jobId - UUID of the job to cancel.
   */
  async cancel(jobId: string): Promise<void> {
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/jobs/${encodeURIComponent(jobId)}/cancel`
    );
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
    const body: Record<string, unknown> = { progress };
    if (message !== undefined) {
      body.message = message;
    }
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/jobs/${encodeURIComponent(jobId)}/progress`,
      body
    );
  }

  /**
   * Send a heartbeat for an active job to prevent stall detection.
   *
   * Workers processing long-running jobs should call this periodically.
   *
   * @param jobId - UUID of the active job.
   */
  async heartbeat(jobId: string): Promise<void> {
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/jobs/${encodeURIComponent(jobId)}/heartbeat`
    );
  }

  /**
   * Get full details for a specific job.
   *
   * @param jobId - UUID of the job.
   * @returns The job object, or `null` if not found.
   */
  async getJob(jobId: string): Promise<Job | null> {
    try {
      const resp = await this.request<{ job: Job }>(
        "GET",
        `/api/v1/jobs/${encodeURIComponent(jobId)}`
      );
      return resp.job;
    } catch (err) {
      if (err instanceof RustQueueError && err.code === "NOT_FOUND") {
        return null;
      }
      throw err;
    }
  }

  // ── Queue Operations ─────────────────────────────────────────────────────

  /**
   * List all queues with their job counts.
   *
   * @returns Array of queue info objects.
   */
  async listQueues(): Promise<QueueInfo[]> {
    const resp = await this.request<{ queues: QueueInfo[] }>(
      "GET",
      "/api/v1/queues"
    );
    return resp.queues;
  }

  /**
   * Get statistics (job counts by state) for a specific queue.
   *
   * @param queue - Queue name.
   * @returns Counts for each job state.
   */
  async getQueueStats(queue: string): Promise<QueueCounts> {
    const resp = await this.request<{ counts: QueueCounts }>(
      "GET",
      `/api/v1/queues/${encodeURIComponent(queue)}/stats`
    );
    return resp.counts;
  }

  /**
   * List dead-letter-queue jobs for a specific queue.
   *
   * @param queue - Queue name.
   * @param limit - Maximum number of DLQ jobs to return. Default: 50.
   * @returns Array of DLQ jobs.
   */
  async getDlqJobs(queue: string, limit?: number): Promise<Job[]> {
    const query = limit !== undefined ? `?limit=${limit}` : "";
    const resp = await this.request<{ jobs: Job[] }>(
      "GET",
      `/api/v1/queues/${encodeURIComponent(queue)}/dlq${query}`
    );
    return resp.jobs;
  }

  // ── Schedule Operations ──────────────────────────────────────────────────

  /**
   * Create or update a schedule.
   *
   * Schedules automatically create jobs at defined intervals (cron or fixed).
   *
   * @param schedule - Schedule definition.
   *
   * @example
   * ```ts
   * await client.createSchedule({
   *   name: "hourly-cleanup",
   *   queue: "maintenance",
   *   job_name: "cleanup-expired",
   *   job_data: { max_age_hours: 24 },
   *   cron_expr: "0 * * * *",
   * });
   * ```
   */
  async createSchedule(schedule: ScheduleInput): Promise<void> {
    await this.request<Record<string, unknown>>(
      "POST",
      "/api/v1/schedules",
      schedule
    );
  }

  /**
   * List all schedules.
   *
   * @returns Array of schedule objects.
   */
  async listSchedules(): Promise<Schedule[]> {
    const resp = await this.request<{ schedules: Schedule[] }>(
      "GET",
      "/api/v1/schedules"
    );
    return resp.schedules;
  }

  /**
   * Get a specific schedule by name.
   *
   * @param name - Schedule name.
   * @returns The schedule object, or `null` if not found.
   */
  async getSchedule(name: string): Promise<Schedule | null> {
    try {
      const resp = await this.request<{ schedule: Schedule }>(
        "GET",
        `/api/v1/schedules/${encodeURIComponent(name)}`
      );
      return resp.schedule;
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
    await this.request<Record<string, unknown>>(
      "DELETE",
      `/api/v1/schedules/${encodeURIComponent(name)}`
    );
  }

  /**
   * Pause a schedule (stops creating new jobs until resumed).
   *
   * @param name - Schedule name.
   */
  async pauseSchedule(name: string): Promise<void> {
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/schedules/${encodeURIComponent(name)}/pause`
    );
  }

  /**
   * Resume a paused schedule.
   *
   * @param name - Schedule name.
   */
  async resumeSchedule(name: string): Promise<void> {
    await this.request<Record<string, unknown>>(
      "POST",
      `/api/v1/schedules/${encodeURIComponent(name)}/resume`
    );
  }

  // ── Health ───────────────────────────────────────────────────────────────

  /**
   * Check server health.
   *
   * @returns Health information including uptime and version.
   */
  async health(): Promise<HealthResponse> {
    const resp = await this.request<HealthResponse>("GET", "/api/v1/health");
    return resp;
  }

  // ── Internal ─────────────────────────────────────────────────────────────

  /**
   * Make an HTTP request to the RustQueue server and parse the response.
   *
   * Throws {@link RustQueueError} if the server returns `{ ok: false }` or
   * if the HTTP status is not 2xx.
   */
  private async request<T>(
    method: string,
    path: string,
    body?: unknown
  ): Promise<T> {
    const url = `${this.baseUrl}${path}`;

    const init: RequestInit = {
      method,
      headers: { ...this.headers },
    };

    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }

    let response: Response;
    try {
      response = await fetch(url, init);
    } catch (err) {
      throw new RustQueueError(
        "NETWORK_ERROR",
        `Failed to connect to ${this.baseUrl}: ${err instanceof Error ? err.message : String(err)}`
      );
    }

    let json: Record<string, unknown>;
    try {
      json = (await response.json()) as Record<string, unknown>;
    } catch {
      throw new RustQueueError(
        "PARSE_ERROR",
        `Invalid JSON response from server (HTTP ${response.status})`,
        response.status
      );
    }

    // Server always returns { ok: true/false, ... }.
    if (json.ok === false) {
      const error = json.error as
        | { code: string; message: string }
        | undefined;
      if (error) {
        throw RustQueueError.fromServerError(error, response.status);
      }
      throw new RustQueueError(
        "UNKNOWN_ERROR",
        `Server returned ok=false without error details (HTTP ${response.status})`,
        response.status
      );
    }

    if (!response.ok) {
      throw new RustQueueError(
        "HTTP_ERROR",
        `HTTP ${response.status}: ${response.statusText}`,
        response.status
      );
    }

    return json as unknown as T;
  }
}
