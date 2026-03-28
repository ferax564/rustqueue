# rustqueue

Official Python SDK for [RustQueue](https://github.com/ferax564/rustqueue) — background jobs without infrastructure.

HTTP client using only Python standard library. Zero dependencies. Python >= 3.8.

## Installation

```bash
pip install rustqueue
```

## Quick Start

```python
from rustqueue import RustQueueClient

rq = RustQueueClient("http://localhost:6790")

# Push a job
job_id = rq.push("emails", "send-welcome", {"to": "user@example.com"})

# Pull and process
jobs = rq.pull("emails", count=1)
print(f"Processing: {jobs[0]['name']}")
rq.ack(jobs[0]["id"])
```

## API

- `push(queue, name, data, **opts)` — Enqueue a job
- `pull(queue, count=1)` — Dequeue jobs
- `ack(job_id, result=None)` — Acknowledge completion
- `fail(job_id, error)` — Report failure
- `cancel(job_id)` — Cancel a waiting job
- `progress(job_id, progress, message=None)` — Update progress (0–100)
- `heartbeat(job_id)` — Send worker heartbeat
- `get_job(job_id)` — Get job details
- `list_queues()` — List all queues
- `queue_stats(queue)` — Queue statistics
- `dlq(queue, limit=20)` — List dead-letter jobs
- `health()` — Health check

### Schedules

- `create_schedule(schedule)` — Create cron/interval schedule
- `list_schedules()` — List all schedules
- `get_schedule(name)` — Get schedule by name
- `delete_schedule(name)` — Delete schedule
- `pause_schedule(name)` / `resume_schedule(name)`

## License

MIT
