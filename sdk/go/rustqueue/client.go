package rustqueue

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
)

// Client is an HTTP client for the RustQueue REST API.
//
// All methods return a *RustQueueError on failure. The error includes
// the server's error code and message when available.
type Client struct {
	baseURL    string
	token      string
	httpClient *http.Client
}

// NewClient creates a new HTTP client for the RustQueue REST API.
//
// The baseURL should be the full URL of the RustQueue HTTP server,
// for example "http://localhost:6790".
func NewClient(baseURL string, opts ...ClientOption) *Client {
	cfg := defaultClientConfig()
	for _, opt := range opts {
		opt(&cfg)
	}

	httpClient := cfg.httpClient
	if httpClient == nil {
		httpClient = &http.Client{Timeout: cfg.timeout}
	}

	return &Client{
		baseURL:    strings.TrimRight(baseURL, "/"),
		token:      cfg.token,
		httpClient: httpClient,
	}
}

// Push pushes a single job to a queue.
// Returns the UUID of the created job.
func (c *Client) Push(ctx context.Context, queue, name string, data interface{}, opts *JobOptions) (string, error) {
	body := buildPushBody(name, data, opts)
	var resp struct {
		ID string `json:"id"`
	}
	if err := c.request(ctx, http.MethodPost, "/api/v1/queues/"+url.PathEscape(queue)+"/jobs", body, &resp); err != nil {
		return "", err
	}
	return resp.ID, nil
}

// PushBatch pushes multiple jobs to a queue in a single request.
// Returns an array of UUIDs for the created jobs, in the same order as the input.
func (c *Client) PushBatch(ctx context.Context, queue string, jobs []PushJobInput) ([]string, error) {
	body := buildBatchPushBody(jobs)
	var resp struct {
		IDs []string `json:"ids"`
	}
	if err := c.request(ctx, http.MethodPost, "/api/v1/queues/"+url.PathEscape(queue)+"/jobs", body, &resp); err != nil {
		return nil, err
	}
	return resp.IDs, nil
}

// Pull pulls one or more jobs from a queue.
// Pulled jobs transition to active state. The worker must eventually call
// Ack or Fail for each pulled job.
// Returns an array of active jobs (may be empty if the queue has no waiting jobs).
func (c *Client) Pull(ctx context.Context, queue string, count int) ([]Job, error) {
	path := "/api/v1/queues/" + url.PathEscape(queue) + "/jobs"
	if count > 1 {
		path += fmt.Sprintf("?count=%d", count)
	}
	var resp struct {
		Job  *Job  `json:"job"`
		Jobs []Job `json:"jobs"`
	}
	if err := c.request(ctx, http.MethodGet, path, nil, &resp); err != nil {
		return nil, err
	}
	if resp.Jobs != nil {
		return resp.Jobs, nil
	}
	if resp.Job != nil {
		return []Job{*resp.Job}, nil
	}
	return []Job{}, nil
}

// Ack acknowledges successful completion of a job.
// The result parameter is optional and may be nil.
func (c *Client) Ack(ctx context.Context, jobID string, result interface{}) error {
	var body interface{}
	if result != nil {
		body = map[string]interface{}{"result": result}
	}
	return c.request(ctx, http.MethodPost, "/api/v1/jobs/"+url.PathEscape(jobID)+"/ack", body, nil)
}

// AckBatch acknowledges multiple jobs in individual HTTP requests.
// Returns an AckBatchResponse summarizing successes and failures.
// Note: Unlike the TCP client which uses a single ack_batch command,
// the HTTP API processes acks individually, so this method makes N requests.
func (c *Client) AckBatch(ctx context.Context, items []AckBatchItem) (*AckBatchResponse, error) {
	results := make([]AckBatchResult, len(items))
	acked := 0
	failed := 0
	for i, item := range items {
		err := c.Ack(ctx, item.ID, item.Result)
		if err != nil {
			failed++
			rqe, ok := IsRustQueueError(err)
			if ok {
				results[i] = AckBatchResult{ID: item.ID, OK: false, Error: &AckBatchResultError{Code: rqe.Code, Message: rqe.Message}}
			} else {
				results[i] = AckBatchResult{ID: item.ID, OK: false, Error: &AckBatchResultError{Code: "UNKNOWN_ERROR", Message: err.Error()}}
			}
		} else {
			acked++
			results[i] = AckBatchResult{ID: item.ID, OK: true}
		}
	}
	return &AckBatchResponse{
		OK:      failed == 0,
		Acked:   acked,
		Failed:  failed,
		Results: results,
	}, nil
}

// Fail reports that a job has failed.
// The server will determine whether to retry (based on max_attempts and backoff)
// or move the job to the dead-letter queue.
func (c *Client) Fail(ctx context.Context, jobID string, errMsg string) (*FailResult, error) {
	body := map[string]interface{}{"error": errMsg}
	var resp FailResult
	if err := c.request(ctx, http.MethodPost, "/api/v1/jobs/"+url.PathEscape(jobID)+"/fail", body, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Cancel cancels a job. Only non-active, non-completed jobs can be cancelled.
func (c *Client) Cancel(ctx context.Context, jobID string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/jobs/"+url.PathEscape(jobID)+"/cancel", nil, nil)
}

// Progress updates the progress of an active job.
// The progress value should be from 0 to 100. The message parameter is optional
// and may be empty.
func (c *Client) Progress(ctx context.Context, jobID string, progress int, message string) error {
	body := map[string]interface{}{"progress": progress}
	if message != "" {
		body["message"] = message
	}
	return c.request(ctx, http.MethodPost, "/api/v1/jobs/"+url.PathEscape(jobID)+"/progress", body, nil)
}

// Heartbeat sends a heartbeat for an active job to prevent stall detection.
// Workers processing long-running jobs should call this periodically.
func (c *Client) Heartbeat(ctx context.Context, jobID string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/jobs/"+url.PathEscape(jobID)+"/heartbeat", nil, nil)
}

// GetJob gets full details for a specific job.
// Returns nil if the job is not found.
func (c *Client) GetJob(ctx context.Context, jobID string) (*Job, error) {
	var resp struct {
		Job Job `json:"job"`
	}
	if err := c.request(ctx, http.MethodGet, "/api/v1/jobs/"+url.PathEscape(jobID), nil, &resp); err != nil {
		if rqe, ok := IsRustQueueError(err); ok && rqe.IsNotFound() {
			return nil, nil
		}
		return nil, err
	}
	return &resp.Job, nil
}

// ListQueues lists all queues with their job counts.
func (c *Client) ListQueues(ctx context.Context) ([]QueueInfo, error) {
	var resp struct {
		Queues []QueueInfo `json:"queues"`
	}
	if err := c.request(ctx, http.MethodGet, "/api/v1/queues", nil, &resp); err != nil {
		return nil, err
	}
	return resp.Queues, nil
}

// GetQueueStats gets statistics (job counts by state) for a specific queue.
func (c *Client) GetQueueStats(ctx context.Context, queue string) (*QueueCounts, error) {
	var resp struct {
		Counts QueueCounts `json:"counts"`
	}
	if err := c.request(ctx, http.MethodGet, "/api/v1/queues/"+url.PathEscape(queue)+"/stats", nil, &resp); err != nil {
		return nil, err
	}
	return &resp.Counts, nil
}

// GetDlqJobs lists dead-letter-queue jobs for a specific queue.
// If limit is 0, the server default (50) is used.
func (c *Client) GetDlqJobs(ctx context.Context, queue string, limit int) ([]Job, error) {
	path := "/api/v1/queues/" + url.PathEscape(queue) + "/dlq"
	if limit > 0 {
		path += fmt.Sprintf("?limit=%d", limit)
	}
	var resp struct {
		Jobs []Job `json:"jobs"`
	}
	if err := c.request(ctx, http.MethodGet, path, nil, &resp); err != nil {
		return nil, err
	}
	return resp.Jobs, nil
}

// CreateSchedule creates or updates a schedule.
// Schedules automatically create jobs at defined intervals (cron or fixed).
func (c *Client) CreateSchedule(ctx context.Context, schedule ScheduleInput) error {
	return c.request(ctx, http.MethodPost, "/api/v1/schedules", schedule, nil)
}

// ListSchedules lists all schedules.
func (c *Client) ListSchedules(ctx context.Context) ([]Schedule, error) {
	var resp struct {
		Schedules []Schedule `json:"schedules"`
	}
	if err := c.request(ctx, http.MethodGet, "/api/v1/schedules", nil, &resp); err != nil {
		return nil, err
	}
	return resp.Schedules, nil
}

// GetSchedule gets a specific schedule by name.
// Returns nil if the schedule is not found.
func (c *Client) GetSchedule(ctx context.Context, name string) (*Schedule, error) {
	var resp struct {
		Schedule Schedule `json:"schedule"`
	}
	if err := c.request(ctx, http.MethodGet, "/api/v1/schedules/"+url.PathEscape(name), nil, &resp); err != nil {
		if rqe, ok := IsRustQueueError(err); ok && rqe.IsNotFound() {
			return nil, nil
		}
		return nil, err
	}
	return &resp.Schedule, nil
}

// DeleteSchedule deletes a schedule.
func (c *Client) DeleteSchedule(ctx context.Context, name string) error {
	return c.request(ctx, http.MethodDelete, "/api/v1/schedules/"+url.PathEscape(name), nil, nil)
}

// PauseSchedule pauses a schedule (stops creating new jobs until resumed).
func (c *Client) PauseSchedule(ctx context.Context, name string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/schedules/"+url.PathEscape(name)+"/pause", nil, nil)
}

// ResumeSchedule resumes a paused schedule.
func (c *Client) ResumeSchedule(ctx context.Context, name string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/schedules/"+url.PathEscape(name)+"/resume", nil, nil)
}

// PauseQueue pauses a queue (rejects new pushes with 503).
func (c *Client) PauseQueue(ctx context.Context, queue string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/queues/"+url.PathEscape(queue)+"/pause", nil, nil)
}

// ResumeQueue resumes a paused queue.
func (c *Client) ResumeQueue(ctx context.Context, queue string) error {
	return c.request(ctx, http.MethodPost, "/api/v1/queues/"+url.PathEscape(queue)+"/resume", nil, nil)
}

// Health checks server health.
func (c *Client) Health(ctx context.Context) (*HealthResponse, error) {
	var resp HealthResponse
	if err := c.request(ctx, http.MethodGet, "/api/v1/health", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// request is the internal HTTP helper. It handles JSON marshaling/unmarshaling,
// auth headers, and error wrapping.
//
// If body is non-nil, it is marshaled to JSON and sent as the request body.
// If dest is non-nil, the response JSON is unmarshaled into it (after stripping
// the top-level "ok" envelope).
func (c *Client) request(ctx context.Context, method, path string, body interface{}, dest interface{}) error {
	reqURL := c.baseURL + path

	var bodyReader io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return &RustQueueError{Code: "MARSHAL_ERROR", Message: fmt.Sprintf("failed to marshal request body: %v", err)}
		}
		bodyReader = bytes.NewReader(data)
	}

	req, err := http.NewRequestWithContext(ctx, method, reqURL, bodyReader)
	if err != nil {
		return &RustQueueError{Code: "REQUEST_ERROR", Message: fmt.Sprintf("failed to create request: %v", err)}
	}

	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return &RustQueueError{Code: "NETWORK_ERROR", Message: fmt.Sprintf("failed to connect to %s: %v", c.baseURL, err)}
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return &RustQueueError{
			Code:       "READ_ERROR",
			Message:    fmt.Sprintf("failed to read response body: %v", err),
			StatusCode: resp.StatusCode,
		}
	}

	// Parse the JSON envelope.
	var envelope map[string]json.RawMessage
	if err := json.Unmarshal(respBody, &envelope); err != nil {
		return &RustQueueError{
			Code:       "PARSE_ERROR",
			Message:    fmt.Sprintf("invalid JSON response from server (HTTP %d)", resp.StatusCode),
			StatusCode: resp.StatusCode,
		}
	}

	// Check for ok=false.
	if okRaw, exists := envelope["ok"]; exists {
		var ok bool
		if err := json.Unmarshal(okRaw, &ok); err == nil && !ok {
			// Extract error details.
			if errRaw, exists := envelope["error"]; exists {
				var serverErr struct {
					Code    string `json:"code"`
					Message string `json:"message"`
				}
				if err := json.Unmarshal(errRaw, &serverErr); err == nil {
					return &RustQueueError{
						Code:       serverErr.Code,
						Message:    serverErr.Message,
						StatusCode: resp.StatusCode,
					}
				}
			}
			return &RustQueueError{
				Code:       "UNKNOWN_ERROR",
				Message:    fmt.Sprintf("server returned ok=false without error details (HTTP %d)", resp.StatusCode),
				StatusCode: resp.StatusCode,
			}
		}
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return &RustQueueError{
			Code:       "HTTP_ERROR",
			Message:    fmt.Sprintf("HTTP %d: %s", resp.StatusCode, resp.Status),
			StatusCode: resp.StatusCode,
		}
	}

	// Unmarshal into destination if provided.
	if dest != nil {
		if err := json.Unmarshal(respBody, dest); err != nil {
			return &RustQueueError{
				Code:       "PARSE_ERROR",
				Message:    fmt.Sprintf("failed to parse response body: %v", err),
				StatusCode: resp.StatusCode,
			}
		}
	}

	return nil
}
