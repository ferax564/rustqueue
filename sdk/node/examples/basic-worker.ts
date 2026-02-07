/**
 * Basic RustQueue worker example.
 *
 * Demonstrates the fundamental push -> pull -> process -> ack loop
 * using both HTTP and TCP clients.
 *
 * Prerequisites:
 *   - RustQueue server running on default ports (HTTP 6790, TCP 6789)
 *   - `cargo run -- serve` from the project root
 *
 * Usage:
 *   npx tsx examples/basic-worker.ts
 */

import {
  RustQueueClient,
  RustQueueTcpClient,
  RustQueueError,
} from "../src/index.js";
import type { Job } from "../src/index.js";

// ── Configuration ────────────────────────────────────────────────────────────

const HTTP_URL = process.env.RUSTQUEUE_HTTP_URL ?? "http://localhost:6790";
const TCP_HOST = process.env.RUSTQUEUE_TCP_HOST ?? "127.0.0.1";
const TCP_PORT = parseInt(process.env.RUSTQUEUE_TCP_PORT ?? "6789", 10);
const QUEUE = "example-tasks";

// ── HTTP Client Example ──────────────────────────────────────────────────────

async function httpExample(): Promise<void> {
  console.log("=== HTTP Client Example ===\n");

  const client = new RustQueueClient({ baseUrl: HTTP_URL });

  // Check server health.
  const health = await client.health();
  console.log(`Server status: ${health.status} (v${health.version}), uptime: ${health.uptime_seconds}s`);

  // Push a single job.
  const jobId = await client.push(QUEUE, "send-email", {
    to: "alice@example.com",
    subject: "Welcome!",
    template: "welcome",
  }, {
    priority: 5,
    max_attempts: 3,
    timeout_ms: 30_000,
  });
  console.log(`Pushed job: ${jobId}`);

  // Push a batch of jobs.
  const batchIds = await client.pushBatch(QUEUE, [
    { name: "send-email", data: { to: "bob@example.com", subject: "Hello" } },
    { name: "send-email", data: { to: "carol@example.com", subject: "Hi" } },
    {
      name: "send-sms",
      data: { phone: "+1234567890", body: "Your code is 1234" },
      options: { priority: 10 },
    },
  ]);
  console.log(`Pushed batch: ${batchIds.join(", ")}`);

  // Check queue stats.
  const stats = await client.getQueueStats(QUEUE);
  console.log(`Queue stats — waiting: ${stats.waiting}, active: ${stats.active}`);

  // Pull and process jobs one at a time.
  console.log("\nProcessing jobs...\n");

  let processed = 0;
  while (processed < 4) {
    const jobs = await client.pull(QUEUE);
    if (jobs.length === 0) {
      console.log("No more jobs in queue.");
      break;
    }

    for (const job of jobs) {
      await processJob(client, job);
      processed++;
    }
  }

  // List queues.
  const queues = await client.listQueues();
  console.log(`\nQueues: ${queues.map((q) => `${q.name} (${q.counts.completed} completed)`).join(", ")}`);

  console.log("\nHTTP example done.\n");
}

/**
 * Simulate processing a job with progress updates.
 */
async function processJob(client: RustQueueClient, job: Job): Promise<void> {
  console.log(`  Processing [${job.name}] ${job.id}...`);

  try {
    // Report progress.
    await client.progress(job.id, 25, "Starting...");
    await client.progress(job.id, 50, "Halfway there...");
    await client.progress(job.id, 75, "Almost done...");

    // Simulate work.
    // In a real worker, this would be the actual job logic.
    const result = { sent: true, timestamp: new Date().toISOString() };

    // Acknowledge completion.
    await client.ack(job.id, result);
    console.log(`  Completed [${job.name}] ${job.id}`);
  } catch (err) {
    if (err instanceof RustQueueError) {
      console.error(`  Failed [${job.name}] ${job.id}: [${err.code}] ${err.message}`);
    } else {
      // Report failure to the server so it can retry.
      const errMessage = err instanceof Error ? err.message : String(err);
      await client.fail(job.id, errMessage);
      console.error(`  Failed [${job.name}] ${job.id}: ${errMessage}`);
    }
  }
}

// ── TCP Client Example ───────────────────────────────────────────────────────

async function tcpExample(): Promise<void> {
  console.log("=== TCP Client Example ===\n");

  const tcp = new RustQueueTcpClient({
    host: TCP_HOST,
    port: TCP_PORT,
    autoReconnect: true,
    maxReconnectAttempts: 3,
  });

  try {
    await tcp.connect();
    console.log(`Connected to TCP server at ${TCP_HOST}:${TCP_PORT}`);

    // Push jobs over TCP (lower overhead than HTTP).
    const jobId = await tcp.push(QUEUE, "tcp-task", {
      action: "process-data",
      payload: [1, 2, 3],
    });
    console.log(`Pushed job via TCP: ${jobId}`);

    // Batch push.
    const batchIds = await tcp.pushBatch(QUEUE, [
      { name: "tcp-task", data: { action: "batch-1" } },
      { name: "tcp-task", data: { action: "batch-2" } },
    ]);
    console.log(`Pushed batch via TCP: ${batchIds.join(", ")}`);

    // Pull and ack in a tight loop.
    let processed = 0;
    while (processed < 3) {
      const jobs = await tcp.pull(QUEUE);
      if (jobs.length === 0) {
        break;
      }

      for (const job of jobs) {
        console.log(`  Processing [${job.name}] ${job.id} via TCP...`);
        await tcp.ack(job.id, { processed: true });
        console.log(`  Completed [${job.name}] ${job.id}`);
        processed++;
      }
    }

    // Get queue stats.
    const stats = await tcp.getQueueStats(QUEUE);
    console.log(`\nQueue stats — waiting: ${stats.waiting}, completed: ${stats.completed}`);

    console.log("\nTCP example done.\n");
  } finally {
    tcp.disconnect();
  }
}

// ── Schedule Example ─────────────────────────────────────────────────────────

async function scheduleExample(): Promise<void> {
  console.log("=== Schedule Example ===\n");

  const client = new RustQueueClient({ baseUrl: HTTP_URL });

  // Create a schedule that runs every 60 seconds.
  await client.createSchedule({
    name: "periodic-cleanup",
    queue: "maintenance",
    job_name: "cleanup-expired-sessions",
    job_data: { max_age_hours: 24 },
    every_ms: 60_000,
    max_executions: 100,
  });
  console.log("Created schedule: periodic-cleanup");

  // List all schedules.
  const schedules = await client.listSchedules();
  for (const s of schedules) {
    console.log(`  Schedule: ${s.name} (queue: ${s.queue}, paused: ${s.paused})`);
  }

  // Pause the schedule.
  await client.pauseSchedule("periodic-cleanup");
  console.log("Paused schedule: periodic-cleanup");

  // Resume the schedule.
  await client.resumeSchedule("periodic-cleanup");
  console.log("Resumed schedule: periodic-cleanup");

  // Delete the schedule.
  await client.deleteSchedule("periodic-cleanup");
  console.log("Deleted schedule: periodic-cleanup");

  console.log("\nSchedule example done.\n");
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  try {
    await httpExample();
    await tcpExample();
    await scheduleExample();
  } catch (err) {
    if (err instanceof RustQueueError) {
      console.error(`\nRustQueue error: [${err.code}] ${err.message}`);
    } else {
      console.error("\nUnexpected error:", err);
    }
    process.exit(1);
  }
}

main();
