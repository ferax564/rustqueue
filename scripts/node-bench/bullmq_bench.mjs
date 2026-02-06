import { performance } from "node:perf_hooks";
import { Queue, Worker, QueueEvents } from "bullmq";
import IORedis from "ioredis";

function argValue(name, fallback) {
  const idx = process.argv.indexOf(name);
  if (idx >= 0 && idx + 1 < process.argv.length) {
    return process.argv[idx + 1];
  }
  return fallback;
}

function opsPerS(n, seconds) {
  return seconds > 0 ? n / seconds : 0;
}

function withTimeout(promise, timeoutMs, message) {
  let timer = null;
  const timeoutPromise = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs);
  });
  return Promise.race([promise, timeoutPromise]).finally(() => {
    if (timer !== null) {
      clearTimeout(timer);
    }
  });
}

async function main() {
  const ops = Number(argValue("--ops", "1000"));
  const redisPort = Number(argValue("--redis-port", "6380"));
  const suffix = Date.now();

  const connection = new IORedis({
    host: "127.0.0.1",
    port: redisPort,
    maxRetriesPerRequest: null,
    enableReadyCheck: true,
  });

  try {
    // Produce benchmark
    {
      const queueName = `bench-bullmq-produce-${suffix}`;
      const queue = new Queue(queueName, { connection, defaultJobOptions: { removeOnComplete: true, removeOnFail: true } });

      const t0 = performance.now();
      for (let i = 0; i < ops; i += 1) {
        await queue.add("bench", { i });
      }
      const produceSec = (performance.now() - t0) / 1000;

      await queue.drain(true);
      await queue.obliterate({ force: true });
      await queue.close();

      globalThis.__produce = opsPerS(ops, produceSec);
    }

    // Consume benchmark
    {
      const queueName = `bench-bullmq-consume-${suffix}`;
      const queue = new Queue(queueName, { connection, defaultJobOptions: { removeOnComplete: true, removeOnFail: true } });

      for (let i = 0; i < ops; i += 1) {
        await queue.add("bench", { i });
      }

      let completed = 0;
      let resolveDone = null;
      let rejectDone = null;
      const donePromise = new Promise((resolve, reject) => {
        resolveDone = resolve;
        rejectDone = reject;
      });

      const worker = new Worker(
        queueName,
        async (job) => job.data.i,
        {
          connection,
          concurrency: 1,
        },
      );
      worker.on("completed", () => {
        completed += 1;
        if (completed >= ops && resolveDone) {
          resolveDone();
        }
      });
      worker.on("failed", (job, err) => {
        if (rejectDone) {
          rejectDone(err || new Error(`job ${job?.id ?? "unknown"} failed`));
        }
      });

      await worker.waitUntilReady();

      const t0 = performance.now();
      await withTimeout(donePromise, 120000, "timeout waiting for BullMQ consume completion");
      const consumeSec = (performance.now() - t0) / 1000;

      await worker.close();
      await queue.drain(true);
      await queue.obliterate({ force: true });
      await queue.close();

      globalThis.__consume = opsPerS(ops, consumeSec);
    }

    // End-to-end benchmark
    {
      const queueName = `bench-bullmq-e2e-${suffix}`;
      const queue = new Queue(queueName, { connection, defaultJobOptions: { removeOnComplete: true, removeOnFail: true } });
      const queueEvents = new QueueEvents(queueName, { connection });
      await queueEvents.waitUntilReady();

      const worker = new Worker(
        queueName,
        async (job) => job.data.i,
        {
          connection,
          concurrency: 1,
        },
      );
      await worker.waitUntilReady();

      const t0 = performance.now();
      for (let i = 0; i < ops; i += 1) {
        const job = await queue.add("bench", { i });
        await job.waitUntilFinished(queueEvents);
      }
      const e2eSec = (performance.now() - t0) / 1000;

      await worker.close();
      await queueEvents.close();
      await queue.drain(true);
      await queue.obliterate({ force: true });
      await queue.close();

      globalThis.__e2e = opsPerS(ops, e2eSec);
    }

    console.log(
      JSON.stringify({
        system: "bullmq",
        produce_ops_s: globalThis.__produce,
        consume_ops_s: globalThis.__consume,
        end_to_end_ops_s: globalThis.__e2e,
      }),
    );
  } finally {
    await connection.quit();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
