// Package rustqueue provides Go clients for the RustQueue distributed job scheduler.
//
// Both HTTP and TCP clients are available. The HTTP client is simpler and suitable
// for most use cases. The TCP client provides lower overhead for high-throughput
// worker patterns.
//
// All types use snake_case JSON field names to match the RustQueue wire format.
package rustqueue

import "encoding/json"

// JobState represents all possible states in a job's lifecycle.
type JobState string

const (
	JobStateCreated   JobState = "created"
	JobStateWaiting   JobState = "waiting"
	JobStateDelayed   JobState = "delayed"
	JobStateActive    JobState = "active"
	JobStateCompleted JobState = "completed"
	JobStateFailed    JobState = "failed"
	JobStateDlq       JobState = "dlq"
	JobStateCancelled JobState = "cancelled"
	JobStateBlocked   JobState = "blocked"
)

// BackoffStrategy defines how delay increases between retry attempts.
type BackoffStrategy string

const (
	// BackoffFixed uses the same delay for every retry.
	BackoffFixed BackoffStrategy = "fixed"
	// BackoffLinear multiplies delay by the attempt number.
	BackoffLinear BackoffStrategy = "linear"
	// BackoffExponential multiplies delay by 2^attempt (default).
	BackoffExponential BackoffStrategy = "exponential"
)

// LogEntry is a single log entry appended by a worker during job processing.
type LogEntry struct {
	Timestamp string `json:"timestamp"`
	Message   string `json:"message"`
}

// Job is a unit of work submitted to a queue for processing by a worker.
// This is the full job object as returned by the server.
type Job struct {
	// Identity
	ID       string  `json:"id"`
	CustomID *string `json:"custom_id"`
	Name     string  `json:"name"`
	Queue    string  `json:"queue"`

	// Payload
	Data   interface{} `json:"data"`
	Result interface{} `json:"result"`

	// State
	State    JobState   `json:"state"`
	Progress *float64   `json:"progress"`
	Logs     []LogEntry `json:"logs"`

	// Scheduling
	Priority    int     `json:"priority"`
	DelayUntil  *string `json:"delay_until"`
	CreatedAt   string  `json:"created_at"`
	UpdatedAt   string  `json:"updated_at"`
	StartedAt   *string `json:"started_at"`
	CompletedAt *string `json:"completed_at"`

	// Retry configuration
	MaxAttempts    int             `json:"max_attempts"`
	Attempt        int             `json:"attempt"`
	Backoff        BackoffStrategy `json:"backoff"`
	BackoffDelayMs int             `json:"backoff_delay_ms"`
	LastError      *string         `json:"last_error"`

	// Constraints
	TTLMs     *int    `json:"ttl_ms"`
	TimeoutMs *int    `json:"timeout_ms"`
	UniqueKey *string `json:"unique_key"`

	// Organization
	Tags    []string `json:"tags"`
	GroupID *string  `json:"group_id"`

	// Dependencies
	DependsOn []string `json:"depends_on"`
	FlowID    *string  `json:"flow_id"`

	// Behavior flags
	LIFO             bool `json:"lifo"`
	RemoveOnComplete bool `json:"remove_on_complete"`
	RemoveOnFail     bool `json:"remove_on_fail"`

	// Worker assignment
	WorkerID      *string `json:"worker_id"`
	LastHeartbeat *string `json:"last_heartbeat"`
}

// JobOptions contains optional parameters when pushing a job.
// All fields are optional; the server applies sensible defaults for omitted values.
type JobOptions struct {
	Priority       *int             `json:"priority,omitempty"`
	DelayMs        *int             `json:"delay_ms,omitempty"`
	MaxAttempts    *int             `json:"max_attempts,omitempty"`
	Backoff        *BackoffStrategy `json:"backoff,omitempty"`
	BackoffDelayMs *int             `json:"backoff_delay_ms,omitempty"`
	TTLMs          *int             `json:"ttl_ms,omitempty"`
	TimeoutMs      *int             `json:"timeout_ms,omitempty"`
	UniqueKey      *string          `json:"unique_key,omitempty"`
	Tags           []string         `json:"tags,omitempty"`
	GroupID        *string          `json:"group_id,omitempty"`
	LIFO           *bool            `json:"lifo,omitempty"`
	RemoveOnComplete *bool          `json:"remove_on_complete,omitempty"`
	RemoveOnFail   *bool            `json:"remove_on_fail,omitempty"`
	CustomID       *string          `json:"custom_id,omitempty"`
}

// PushJobInput is the input for pushing a single job in a batch operation.
type PushJobInput struct {
	Name    string      `json:"name"`
	Data    interface{} `json:"data"`
	Options *JobOptions `json:"options,omitempty"`
}

// QueueCounts contains summary counts for a queue, broken down by job state.
type QueueCounts struct {
	Waiting   int `json:"waiting"`
	Active    int `json:"active"`
	Delayed   int `json:"delayed"`
	Completed int `json:"completed"`
	Failed    int `json:"failed"`
	Dlq       int `json:"dlq"`
}

// QueueInfo contains high-level information about a single queue.
type QueueInfo struct {
	Name   string      `json:"name"`
	Counts QueueCounts `json:"counts"`
}

// ScheduleInput is the input for creating or updating a schedule.
// Either CronExpr or EveryMs (or both) must be provided.
type ScheduleInput struct {
	Name          string      `json:"name"`
	Queue         string      `json:"queue"`
	JobName       string      `json:"job_name"`
	JobData       interface{} `json:"job_data,omitempty"`
	JobOptions    *JobOptions `json:"job_options,omitempty"`
	CronExpr      *string     `json:"cron_expr,omitempty"`
	EveryMs       *int        `json:"every_ms,omitempty"`
	Timezone      *string     `json:"timezone,omitempty"`
	MaxExecutions *int        `json:"max_executions,omitempty"`
}

// Schedule is the full schedule object as returned by the server.
type Schedule struct {
	Name       string      `json:"name"`
	Queue      string      `json:"queue"`
	JobName    string      `json:"job_name"`
	JobData    interface{} `json:"job_data"`
	JobOptions *JobOptions `json:"job_options"`

	CronExpr *string `json:"cron_expr"`
	EveryMs  *int    `json:"every_ms"`
	Timezone *string `json:"timezone"`

	MaxExecutions  *int `json:"max_executions"`
	ExecutionCount int  `json:"execution_count"`
	Paused         bool `json:"paused"`

	LastRunAt *string `json:"last_run_at"`
	NextRunAt *string `json:"next_run_at"`
	CreatedAt string  `json:"created_at"`
	UpdatedAt string  `json:"updated_at"`
}

// HealthResponse is the response from the health check endpoint.
type HealthResponse struct {
	OK            bool    `json:"ok"`
	Status        string  `json:"status"`
	Version       string  `json:"version"`
	UptimeSeconds float64 `json:"uptime_seconds"`
}

// FailResult is the response from reporting a job failure.
type FailResult struct {
	Retry         bool    `json:"retry"`
	NextAttemptAt *string `json:"next_attempt_at"`
}

// AckBatchItem is a single item in a batch ack request.
type AckBatchItem struct {
	ID     string      `json:"id"`
	Result interface{} `json:"result,omitempty"`
}

// AckBatchResultError contains error details for a failed batch ack item.
type AckBatchResultError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// AckBatchResult is the result for a single item in a batch ack response.
type AckBatchResult struct {
	ID    string               `json:"id"`
	OK    bool                 `json:"ok"`
	Error *AckBatchResultError `json:"error,omitempty"`
}

// AckBatchResponse is the full response from a batch ack operation.
type AckBatchResponse struct {
	OK      bool             `json:"ok"`
	Acked   int              `json:"acked"`
	Failed  int              `json:"failed"`
	Results []AckBatchResult `json:"results"`
}

// pushBody builds the HTTP request body for a single push operation.
// It flattens JobOptions into the top level to match the server's serde(flatten) behavior.
func buildPushBody(name string, data interface{}, opts *JobOptions) map[string]interface{} {
	body := map[string]interface{}{
		"name": name,
		"data": data,
	}
	if opts == nil {
		return body
	}
	// Marshal options and merge into body to match the server's flatten behavior.
	raw, err := json.Marshal(opts)
	if err != nil {
		return body
	}
	var optMap map[string]interface{}
	if err := json.Unmarshal(raw, &optMap); err != nil {
		return body
	}
	for k, v := range optMap {
		body[k] = v
	}
	return body
}

// buildBatchPushBody builds the HTTP request body for a batch push operation.
func buildBatchPushBody(jobs []PushJobInput) []map[string]interface{} {
	result := make([]map[string]interface{}, len(jobs))
	for i, job := range jobs {
		result[i] = buildPushBody(job.Name, job.Data, job.Options)
	}
	return result
}
