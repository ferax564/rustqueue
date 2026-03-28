# @rustqueue/client

Official Node.js SDK for [RustQueue](https://github.com/ferax564/rustqueue) — background jobs without infrastructure.

Provides both HTTP and TCP clients. Zero runtime dependencies. Node.js >= 18.

## Installation

```bash
npm install @rustqueue/client
```

## Quick Start (HTTP)

```typescript
import { RustQueueClient } from "@rustqueue/client";

const rq = new RustQueueClient("http://localhost:6790");

// Push a job
const { id } = await rq.push("emails", "send-welcome", { to: "user@example.com" });

// Pull and process
const [job] = await rq.pull("emails", 1);
console.log(`Processing: ${job.name}`);
await rq.ack(job.id);
```

## Quick Start (TCP)

```typescript
import { RustQueueTcpClient } from "@rustqueue/client";

const rq = new RustQueueTcpClient("localhost", 6789);
await rq.connect();

const { id } = await rq.push("emails", "send-welcome", { to: "user@example.com" });
const [job] = await rq.pull("emails", 1);
await rq.ack(job.id);

rq.close();
```

## API

Both clients support the full RustQueue API:

- `push(queue, name, data, opts?)` — Enqueue a job
- `pull(queue, count?)` — Dequeue jobs
- `ack(id, result?)` — Acknowledge completion
- `fail(id, error)` — Report failure
- `cancel(id)` — Cancel a waiting job
- `progress(id, progress, message?)` — Update progress (0–100)
- `heartbeat(id)` — Send worker heartbeat
- `getJob(id)` — Get job details
- `listQueues()` — List all queues
- `queueStats(queue)` — Queue statistics
- `dlq(queue, limit?)` — List dead-letter jobs
- `health()` — Health check

### Schedules

- `createSchedule(schedule)` — Create cron/interval schedule
- `listSchedules()` — List all schedules
- `getSchedule(name)` — Get schedule by name
- `deleteSchedule(name)` — Delete schedule
- `pauseSchedule(name)` / `resumeSchedule(name)`

## License

MIT
