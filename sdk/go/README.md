# RustQueue Go SDK

Official Go client for [RustQueue](https://github.com/rustqueue/rustqueue) — background jobs without infrastructure.

Provides both HTTP and TCP clients. Zero external dependencies -- only Go standard library.

## Installation

```bash
go get github.com/rustqueue/rustqueue-go
```

Requires Go 1.21 or later.

## HTTP Client

The HTTP client is the simplest way to interact with RustQueue. It wraps the REST API with typed Go methods.

### Quick Start

```go
package main

import (
    "context"
    "fmt"
    "log"

    "github.com/rustqueue/rustqueue-go/rustqueue"
)

func main() {
    client := rustqueue.NewClient("http://localhost:6790")
    ctx := context.Background()

    // Push a job
    jobID, err := client.Push(ctx, "emails", "send-welcome", map[string]string{
        "to": "alice@example.com",
    }, nil)
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("Pushed job: %s\n", jobID)

    // Pull and process
    jobs, err := client.Pull(ctx, "emails", 1)
    if err != nil {
        log.Fatal(err)
    }
    if len(jobs) > 0 {
        // Process the job...
        err = client.Ack(ctx, jobs[0].ID, map[string]bool{"sent": true})
        if err != nil {
            log.Fatal(err)
        }
    }
}
```

### Configuration

```go
// With authentication
client := rustqueue.NewClient("http://localhost:6790",
    rustqueue.WithToken("my-secret-token"),
)

// With custom timeout
client := rustqueue.NewClient("http://localhost:6790",
    rustqueue.WithTimeout(10 * time.Second),
)

// With custom HTTP client
httpClient := &http.Client{Timeout: 5 * time.Second}
client := rustqueue.NewClient("http://localhost:6790",
    rustqueue.WithHTTPClient(httpClient),
)
```

### Job Operations

```go
ctx := context.Background()

// Push with options
priority := 10
maxAttempts := 5
jobID, _ := client.Push(ctx, "emails", "send", data, &rustqueue.JobOptions{
    Priority:    &priority,
    MaxAttempts: &maxAttempts,
})

// Push batch
ids, _ := client.PushBatch(ctx, "emails", []rustqueue.PushJobInput{
    {Name: "send-welcome", Data: map[string]string{"to": "alice@example.com"}},
    {Name: "send-welcome", Data: map[string]string{"to": "bob@example.com"}},
})

// Pull multiple jobs
jobs, _ := client.Pull(ctx, "emails", 5)

// Acknowledge completion
client.Ack(ctx, jobID, resultData)

// Report failure
result, _ := client.Fail(ctx, jobID, "something went wrong")
// result.Retry tells you if the job will be retried

// Cancel a job
client.Cancel(ctx, jobID)

// Update progress (0-100)
client.Progress(ctx, jobID, 50, "halfway done")

// Send heartbeat
client.Heartbeat(ctx, jobID)

// Get job details
job, _ := client.GetJob(ctx, jobID) // returns nil if not found
```

### Queue Operations

```go
// List all queues
queues, _ := client.ListQueues(ctx)

// Get queue statistics
stats, _ := client.GetQueueStats(ctx, "emails")
fmt.Printf("Waiting: %d, Active: %d\n", stats.Waiting, stats.Active)

// Get dead-letter queue jobs
dlqJobs, _ := client.GetDlqJobs(ctx, "emails", 50)

// Pause/resume a queue
client.PauseQueue(ctx, "emails")
client.ResumeQueue(ctx, "emails")
```

### Schedule Operations

```go
// Create a cron schedule
cron := "0 * * * *"
client.CreateSchedule(ctx, rustqueue.ScheduleInput{
    Name:     "hourly-cleanup",
    Queue:    "maintenance",
    JobName:  "cleanup-expired",
    JobData:  map[string]int{"max_age_hours": 24},
    CronExpr: &cron,
})

// Create an interval schedule
everyMs := 30000
client.CreateSchedule(ctx, rustqueue.ScheduleInput{
    Name:    "health-check",
    Queue:   "monitoring",
    JobName: "ping",
    EveryMs: &everyMs,
})

// List, get, delete, pause, resume
schedules, _ := client.ListSchedules(ctx)
schedule, _ := client.GetSchedule(ctx, "hourly-cleanup") // returns nil if not found
client.PauseSchedule(ctx, "hourly-cleanup")
client.ResumeSchedule(ctx, "hourly-cleanup")
client.DeleteSchedule(ctx, "hourly-cleanup")
```

### Health Check

```go
health, _ := client.Health(ctx)
fmt.Printf("Status: %s, Version: %s, Uptime: %.0fs\n",
    health.Status, health.Version, health.UptimeSeconds)
```

## TCP Client

The TCP client provides lower overhead for high-throughput worker patterns.
It uses newline-delimited JSON over a persistent TCP connection.

### Quick Start

```go
package main

import (
    "fmt"
    "log"

    "github.com/rustqueue/rustqueue-go/rustqueue"
)

func main() {
    client := rustqueue.NewTcpClient("127.0.0.1", 6789)
    if err := client.Connect(); err != nil {
        log.Fatal(err)
    }
    defer client.Disconnect()

    // Push a job
    jobID, _ := client.Push("emails", "send-welcome", map[string]string{
        "to": "alice@example.com",
    }, nil)
    fmt.Printf("Pushed: %s\n", jobID)

    // Pull and ack
    jobs, _ := client.Pull("emails", 1)
    if len(jobs) > 0 {
        client.Ack(jobs[0].ID, nil)
    }
}
```

### Configuration

```go
client := rustqueue.NewTcpClient("127.0.0.1", 6789,
    rustqueue.WithTcpToken("my-secret-token"),
    rustqueue.WithAutoReconnect(true),
    rustqueue.WithMaxReconnectAttempts(20),
    rustqueue.WithReconnectDelay(500), // base delay in ms (exponential backoff)
)
```

### TCP-Only Operations

```go
// Batch ack (TCP only)
resp, _ := client.AckBatch([]rustqueue.AckBatchItem{
    {ID: "job-1", Result: map[string]bool{"ok": true}},
    {ID: "job-2"},
})
fmt.Printf("Acked: %d, Failed: %d\n", resp.Acked, resp.Failed)
```

### Auto-Reconnect

The TCP client supports automatic reconnection with exponential backoff.
When the connection drops, pending requests are rejected with a `CONNECTION_LOST`
error, and the client attempts to reconnect in the background. New requests
will succeed once the connection is re-established.

## Error Handling

All methods return `*rustqueue.RustQueueError` on failure:

```go
job, err := client.GetJob(ctx, "nonexistent-id")
if err != nil {
    if rqe, ok := rustqueue.IsRustQueueError(err); ok {
        fmt.Printf("Code: %s, Message: %s, HTTP: %d\n",
            rqe.Code, rqe.Message, rqe.StatusCode)
        if rqe.IsNotFound() {
            // Handle not found
        }
    }
}
```

Note that `GetJob` and `GetSchedule` return `(nil, nil)` for not-found resources
instead of an error, matching the Node.js SDK behavior.

## Type Reference

| Go Type | JSON Wire Format | Description |
|---------|-----------------|-------------|
| `JobState` | `string` | Job lifecycle state constant |
| `BackoffStrategy` | `string` | Retry backoff strategy constant |
| `Job` | object | Full job with all fields |
| `JobOptions` | object | Optional push parameters (omitempty) |
| `PushJobInput` | object | Batch push item |
| `QueueCounts` | object | Per-state job counts |
| `QueueInfo` | object | Queue name + counts |
| `Schedule` | object | Full schedule object |
| `ScheduleInput` | object | Schedule creation input |
| `HealthResponse` | object | Server health info |
| `FailResult` | object | Fail response (retry + next_attempt_at) |
| `AckBatchItem` | object | Batch ack request item |
| `AckBatchResponse` | object | Batch ack response |

## Examples

See the `examples/` directory:

- `examples/http_producer/` -- Push jobs and check queue stats via HTTP
- `examples/tcp_worker/` -- Pull and process jobs with heartbeat via TCP

## License

Same license as the RustQueue project.
