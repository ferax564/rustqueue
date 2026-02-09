# RustQueue Orchestration API

This document describes the RustQueue API surface relevant to workflow orchestration systems (e.g., ForgePipe). It covers the full job lifecycle, metadata handling, DAG flows, and integration patterns.

## Overview

RustQueue provides a dual-protocol (HTTP REST + TCP) job queue with support for:

- **Job metadata**: Arbitrary JSON attached to jobs, separate from the job data payload
- **DAG flows**: Job dependency graphs with automatic resolution and cascade failure
- **Workflow runtime primitives**: `WorkflowStep` trait + sequential and fan-out/fan-in execution
- **Plugin worker routing**: Register engine worker factories and dispatch by queue or metadata
- **Cross-engine chaining**: Auto-enqueue follow-up jobs on completion
- **Webhooks**: Real-time notifications for job lifecycle events
- **Batch operations**: Push and acknowledge multiple jobs in a single call

## Job Metadata

The `metadata` field on jobs carries orchestration context through the full lifecycle. It is:

- **Separate from `data`**: `data` is the job's input payload; `metadata` is orchestration context
- **Lifecycle-safe**: Survives push, pull, ack, fail, cancel, and retry
- **Optional**: Omit it entirely for backward compatibility (zero overhead when unused)
- **Size-limited**: Maximum 64 KB per job

### Example: ForgePipe Workflow Metadata

```json
{
  "queue": "build",
  "name": "compile-project",
  "data": {"repo": "acme/app", "branch": "main"},
  "options": {
    "metadata": {
      "workflow_id": "forgepipe-run-001",
      "step_id": "build",
      "artifact_ref": "s3://forgepipe-artifacts/run-001/build.tar.gz",
      "parent_step": null,
      "labels": {"env": "staging", "team": "platform"}
    },
    "flow_id": "forgepipe-run-001"
  }
}
```

## Job Lifecycle API

### Push (Enqueue)

**HTTP:** `POST /api/v1/queues/{queue}/jobs`

```json
{
  "name": "compile-project",
  "data": {"repo": "acme/app"},
  "options": {
    "metadata": {"workflow_id": "wf-1", "step_id": "build"},
    "priority": 10,
    "timeout_ms": 300000,
    "max_attempts": 3,
    "flow_id": "wf-1",
    "depends_on": ["<parent-job-uuid>"]
  }
}
```

**TCP:** `{"cmd": "push", "queue": "build", "name": "compile-project", "data": {...}, "options": {...}}`

**Response:** `{"id": "<uuid-v7>"}`

**Batch push (HTTP):** Send an array of job objects in the request body.

**Batch push (TCP):** `{"cmd": "push_batch", "queue": "build", "jobs": [...]}`

### Pull (Dequeue)

**HTTP:** `GET /api/v1/queues/{queue}/jobs?count=5`

**TCP:** `{"cmd": "pull", "queue": "build", "count": 5}`

Returns jobs transitioned to `Active` state. The `metadata` field is included in the response.

### Acknowledge (Complete)

**HTTP:** `POST /api/v1/jobs/{id}/ack`

```json
{
  "result": {"exit_code": 0, "output_path": "/tmp/build.tar.gz"}
}
```

**TCP:** `{"cmd": "ack", "id": "<uuid>", "result": {...}}`

On ack:

1. DAG children in `Blocked` state whose dependencies are now all `Completed` are promoted to `Waiting`.
2. If `metadata.follow_ups` is present, follow-up jobs are automatically enqueued. These jobs inherit `flow_id` (unless explicitly overridden), and `depends_on` is augmented with the parent job id.

**Batch ack (HTTP):** `POST /api/v1/jobs/ack-batch` with `[{"id": "...", "result": {...}}, ...]`

**Batch ack (TCP):** `{"cmd": "ack_batch", "items": [{"id": "...", "result": {...}}]}`

### Fail

**HTTP:** `POST /api/v1/jobs/{id}/fail`

```json
{
  "error": "compilation failed: exit code 1"
}
```

**TCP:** `{"cmd": "fail", "id": "<uuid>", "error": "..."}`

The engine decides whether to retry (based on `max_attempts` and `backoff` strategy) or move to DLQ. If moved to DLQ and the job has DAG children, those children are recursively cascaded to DLQ.

### Cancel

**HTTP:** `POST /api/v1/jobs/{id}/cancel`

**TCP:** `{"cmd": "cancel", "id": "<uuid>"}`

Cancels a job in `Waiting`, `Delayed`, or `Blocked` state.

### Progress

**HTTP:** `POST /api/v1/jobs/{id}/progress`

```json
{
  "progress": 50,
  "message": "Compiling module 3 of 6"
}
```

**TCP:** `{"cmd": "progress", "id": "<uuid>", "progress": 50, "message": "..."}`

Progress is clamped to 0-100. Optional `message` is appended to the job's log entries.

### Heartbeat

**HTTP:** `POST /api/v1/jobs/{id}/heartbeat`

**TCP:** `{"cmd": "heartbeat", "id": "<uuid>"}`

Workers should call this periodically to prevent stall detection from failing the job.

### Get Job

**HTTP:** `GET /api/v1/jobs/{id}`

**TCP:** `{"cmd": "get", "id": "<uuid>"}`

Returns the full job object including `metadata`, `data`, `result`, `state`, `progress`, `logs`, etc.

## DAG Flows

Jobs can declare dependencies via `depends_on` (list of parent job UUIDs) and be grouped via `flow_id`.

### Dependency Resolution

1. **Push with deps**: Job starts as `Blocked` if any parent is not yet `Completed`
2. **Parent completes**: Children whose deps are all `Completed` are promoted to `Waiting`
3. **Parent fails to DLQ**: All `Blocked` children are recursively cascaded to DLQ
4. **Cycle detection**: BFS cycle detection with configurable max depth (default: 10)

### Flow Status

**HTTP:** `GET /api/v1/flows/{flow_id}`

Returns all jobs in the flow with summary counts by state.

## Workflow Runtime (Library API)

For embedding/orchestration scenarios, RustQueue also exposes an in-process workflow runtime:

- `WorkflowStep` trait for engine-specific step execution
- `Workflow` ordered stage execution (`then(...)`)
- Fan-out/fan-in stages (`fan_out(...)`, `fan_out_with_join(...)`)

The output of each stage becomes the input of the next stage.

## Plugin Worker Registry (Library API)

RustQueue supports pluggable workers for multi-engine dispatch:

- Register factories by engine id via `WorkerRegistry::register_engine_factory(...)`
- Map queues to default engines via `route_queue_to_engine(...)`
- Override per job by setting `metadata.engine`
- Dispatch one queued job through the registry via `dispatch_next_with_registered_worker(...)`

Routing precedence:

1. `metadata.engine`
2. queue-to-engine mapping

If no worker route matches, dispatch fails with a validation error and the job remains active until handled.

## Cross-engine Follow-up Chaining

Follow-up jobs can be declared in `metadata.follow_ups`:

```json
{
  "metadata": {
    "follow_ups": [
      {
        "queue": "image-process",
        "name": "render-pages",
        "data": {"doc_id": "d-1"},
        "flow_id": "forgepipe-run-001"
      }
    ]
  }
}
```

Chaining behavior:

1. Parent job is acknowledged.
2. Each follow-up job is pushed automatically.
3. Parent id is added to child `depends_on`.
4. Child `flow_id` uses follow-up `flow_id`, else parent `flow_id`.
5. Parent metadata (excluding `follow_ups`) is inherited when child metadata is omitted.

## Webhooks

Register webhooks to receive HTTP callbacks on job lifecycle events.

**Register:** `POST /api/v1/webhooks`

```json
{
  "url": "https://orchestrator.example.com/hooks/rustqueue",
  "events": ["job.completed", "job.failed", "job.dlq"],
  "queues": ["build", "deploy"],
  "secret": "hmac-signing-secret"
}
```

**Events:** `job.pushed`, `job.completed`, `job.failed`, `job.dlq`, `job.cancelled`, `job.progress`

Webhook payloads are signed with HMAC-SHA256 using the registered secret. Failed deliveries are retried up to 3 times with exponential backoff.

## Real-Time Events (WebSocket)

**HTTP:** `GET /api/v1/events` (WebSocket upgrade)

Streams job lifecycle events in real-time:

```json
{"event": "job.completed", "job_id": "<uuid>", "queue": "build", "timestamp": "..."}
```

## Queue Management

| Endpoint | Description |
|----------|-------------|
| `GET /api/v1/queues` | List all queues with counts |
| `GET /api/v1/queues/{queue}/stats` | Get counts for a specific queue |
| `POST /api/v1/queues/{queue}/pause` | Pause queue (rejects new pushes) |
| `POST /api/v1/queues/{queue}/resume` | Resume paused queue |
| `GET /api/v1/queues/{queue}/dlq` | List dead-letter queue jobs |

## Shared Authentication Middleware

Bearer-token validation is centralized in `rustqueue::auth` and used by both:

- HTTP middleware (`Authorization: Bearer <token>`)
- TCP handshake command (`{"cmd":"auth","token":"..."}`)

This keeps auth semantics identical across protocols.

## Shared Metrics Registry Integration

RustQueue can operate with:

- internally installed Prometheus recorder (`MetricsRegistry::install_default_prometheus_if_unset`)
- externally installed global recorder (`MetricsRegistry::install_external_recorder`)
- externally created Prometheus recorder (`MetricsRegistry::install_external_prometheus_recorder`)

This enables platforms like ForgePipe to unify metrics under one recorder and scrape endpoint.

## Backward Compatibility

- The `metadata` field is optional (`Option<serde_json::Value>`)
- When `None`, the field is omitted from JSON serialization entirely
- Existing clients that don't send `metadata` continue to work unchanged
- The `metadata` field does not affect any job scheduling, dequeue ordering, or state machine logic

## Input Validation Limits

| Field | Max Size |
|-------|----------|
| Queue name | 256 bytes |
| Job name | 1,024 bytes |
| Job data payload | 1 MB |
| Job metadata | 64 KB |
| Unique key | 1,024 bytes |
| Error message | 10 KB |
