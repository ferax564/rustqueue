package rustqueue

import (
	"bufio"
	"encoding/json"
	"fmt"
	"math"
	"net"
	"sync"
	"time"
)

// pendingRequest represents a request waiting for a response from the server.
type pendingRequest struct {
	ch chan tcpResponse
}

// tcpResponse wraps either a successful response or an error.
type tcpResponse struct {
	data map[string]json.RawMessage
	err  error
}

// TcpClient is a TCP client for the RustQueue wire protocol.
//
// The protocol is newline-delimited JSON: each command is a single JSON
// object terminated by \n, and each response is a single JSON object
// terminated by \n. Commands are processed sequentially (FIFO).
//
// All methods return a *RustQueueError on failure.
type TcpClient struct {
	host string
	port int
	cfg  tcpClientConfig

	mu        sync.Mutex // protects writes to conn
	conn      net.Conn
	connected bool
	closing   bool

	pendingMu sync.Mutex
	pending   []*pendingRequest

	reconnectMu       sync.Mutex
	reconnectAttempts  int

	done chan struct{} // closed when readLoop exits
}

// NewTcpClient creates a new TCP client for the RustQueue wire protocol.
func NewTcpClient(host string, port int, opts ...TcpClientOption) *TcpClient {
	cfg := defaultTcpClientConfig()
	for _, opt := range opts {
		opt(&cfg)
	}
	return &TcpClient{
		host: host,
		port: port,
		cfg:  cfg,
	}
}

// Connect opens a TCP connection to the RustQueue server.
// If a token was configured, sends an auth command immediately after connecting.
func (tc *TcpClient) Connect() error {
	tc.mu.Lock()
	if tc.connected {
		tc.mu.Unlock()
		return nil
	}
	tc.closing = false
	tc.mu.Unlock()

	return tc.establishConnection()
}

// Disconnect closes the TCP connection and rejects all pending requests.
// Disables automatic reconnection. Call Connect to re-establish.
func (tc *TcpClient) Disconnect() {
	tc.mu.Lock()
	tc.closing = true
	tc.mu.Unlock()

	tc.cleanup("client disconnected")
}

// IsConnected returns whether the client is currently connected.
func (tc *TcpClient) IsConnected() bool {
	tc.mu.Lock()
	defer tc.mu.Unlock()
	return tc.connected
}

// Push pushes a single job to a queue via TCP.
// Returns the UUID of the created job.
func (tc *TcpClient) Push(queue, name string, data interface{}, opts *JobOptions) (string, error) {
	cmd := map[string]interface{}{
		"cmd":   "push",
		"queue": queue,
		"name":  name,
		"data":  data,
	}
	if opts != nil {
		cmd["options"] = opts
	}
	resp, err := tc.send(cmd)
	if err != nil {
		return "", err
	}
	var id string
	if raw, ok := resp["id"]; ok {
		if err := json.Unmarshal(raw, &id); err != nil {
			return "", &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse job id from response"}
		}
	}
	return id, nil
}

// PushBatch pushes multiple jobs to a queue in a single TCP command.
// Returns an array of UUIDs for the created jobs.
func (tc *TcpClient) PushBatch(queue string, jobs []PushJobInput) ([]string, error) {
	batchJobs := make([]map[string]interface{}, len(jobs))
	for i, job := range jobs {
		entry := map[string]interface{}{
			"name": job.Name,
			"data": job.Data,
		}
		if job.Options != nil {
			entry["options"] = job.Options
		}
		batchJobs[i] = entry
	}
	cmd := map[string]interface{}{
		"cmd":   "push_batch",
		"queue": queue,
		"jobs":  batchJobs,
	}
	resp, err := tc.send(cmd)
	if err != nil {
		return nil, err
	}
	var ids []string
	if raw, ok := resp["ids"]; ok {
		if err := json.Unmarshal(raw, &ids); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse job ids from response"}
		}
	}
	return ids, nil
}

// Pull pulls one or more jobs from a queue via TCP.
// Returns an array of active jobs (may be empty).
func (tc *TcpClient) Pull(queue string, count int) ([]Job, error) {
	cmd := map[string]interface{}{
		"cmd":   "pull",
		"queue": queue,
	}
	if count > 0 {
		cmd["count"] = count
	}
	resp, err := tc.send(cmd)
	if err != nil {
		return nil, err
	}

	// Server returns { job: Job|null } for count<=1, { jobs: Job[] } for count>1.
	if raw, ok := resp["jobs"]; ok {
		var jobs []Job
		if err := json.Unmarshal(raw, &jobs); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse jobs from response"}
		}
		return jobs, nil
	}
	if raw, ok := resp["job"]; ok {
		var job *Job
		if err := json.Unmarshal(raw, &job); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse job from response"}
		}
		if job != nil {
			return []Job{*job}, nil
		}
	}
	return []Job{}, nil
}

// Ack acknowledges successful completion of a job via TCP.
func (tc *TcpClient) Ack(jobID string, result interface{}) error {
	cmd := map[string]interface{}{
		"cmd": "ack",
		"id":  jobID,
	}
	if result != nil {
		cmd["result"] = result
	}
	_, err := tc.send(cmd)
	return err
}

// AckBatch acknowledges multiple jobs in a single TCP command.
func (tc *TcpClient) AckBatch(items []AckBatchItem) (*AckBatchResponse, error) {
	batchItems := make([]map[string]interface{}, len(items))
	for i, item := range items {
		entry := map[string]interface{}{"id": item.ID}
		if item.Result != nil {
			entry["result"] = item.Result
		}
		batchItems[i] = entry
	}
	cmd := map[string]interface{}{
		"cmd":   "ack_batch",
		"items": batchItems,
	}
	resp, err := tc.send(cmd)
	if err != nil {
		return nil, err
	}

	// Reconstruct AckBatchResponse from raw response.
	result := &AckBatchResponse{}
	if raw, ok := resp["ok"]; ok {
		json.Unmarshal(raw, &result.OK)
	}
	if raw, ok := resp["acked"]; ok {
		json.Unmarshal(raw, &result.Acked)
	}
	if raw, ok := resp["failed"]; ok {
		json.Unmarshal(raw, &result.Failed)
	}
	if raw, ok := resp["results"]; ok {
		json.Unmarshal(raw, &result.Results)
	}
	return result, nil
}

// Fail reports that a job has failed via TCP.
func (tc *TcpClient) Fail(jobID string, errMsg string) (*FailResult, error) {
	cmd := map[string]interface{}{
		"cmd":   "fail",
		"id":    jobID,
		"error": errMsg,
	}
	resp, err := tc.send(cmd)
	if err != nil {
		return nil, err
	}
	result := &FailResult{}
	if raw, ok := resp["will_retry"]; ok {
		json.Unmarshal(raw, &result.Retry)
	}
	if raw, ok := resp["next_attempt_at"]; ok {
		var s *string
		json.Unmarshal(raw, &s)
		result.NextAttemptAt = s
	}
	return result, nil
}

// Cancel cancels a job via TCP.
func (tc *TcpClient) Cancel(jobID string) error {
	_, err := tc.send(map[string]interface{}{
		"cmd": "cancel",
		"id":  jobID,
	})
	return err
}

// Progress updates the progress of an active job via TCP.
func (tc *TcpClient) Progress(jobID string, progress int, message string) error {
	cmd := map[string]interface{}{
		"cmd":      "progress",
		"id":       jobID,
		"progress": progress,
	}
	if message != "" {
		cmd["message"] = message
	}
	_, err := tc.send(cmd)
	return err
}

// Heartbeat sends a heartbeat for an active job via TCP.
func (tc *TcpClient) Heartbeat(jobID string) error {
	_, err := tc.send(map[string]interface{}{
		"cmd": "heartbeat",
		"id":  jobID,
	})
	return err
}

// GetJob gets full details for a specific job via TCP.
// Returns nil if the job is not found.
func (tc *TcpClient) GetJob(jobID string) (*Job, error) {
	resp, err := tc.send(map[string]interface{}{
		"cmd": "get",
		"id":  jobID,
	})
	if err != nil {
		if rqe, ok := IsRustQueueError(err); ok && rqe.IsNotFound() {
			return nil, nil
		}
		return nil, err
	}
	if raw, ok := resp["job"]; ok {
		var job *Job
		if err := json.Unmarshal(raw, &job); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse job from response"}
		}
		return job, nil
	}
	return nil, nil
}

// GetQueueStats gets statistics for a specific queue via TCP.
func (tc *TcpClient) GetQueueStats(queue string) (*QueueCounts, error) {
	resp, err := tc.send(map[string]interface{}{
		"cmd":   "stats",
		"queue": queue,
	})
	if err != nil {
		return nil, err
	}
	if raw, ok := resp["counts"]; ok {
		var counts QueueCounts
		if err := json.Unmarshal(raw, &counts); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse queue counts from response"}
		}
		return &counts, nil
	}
	return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "missing counts in response"}
}

// CreateSchedule creates or updates a schedule via TCP.
func (tc *TcpClient) CreateSchedule(schedule ScheduleInput) error {
	cmd := map[string]interface{}{
		"cmd":      "schedule_create",
		"name":     schedule.Name,
		"queue":    schedule.Queue,
		"job_name": schedule.JobName,
	}
	if schedule.JobData != nil {
		cmd["job_data"] = schedule.JobData
	} else {
		cmd["job_data"] = map[string]interface{}{}
	}
	if schedule.JobOptions != nil {
		cmd["job_options"] = schedule.JobOptions
	}
	if schedule.CronExpr != nil {
		cmd["cron_expr"] = *schedule.CronExpr
	}
	if schedule.EveryMs != nil {
		cmd["every_ms"] = *schedule.EveryMs
	}
	if schedule.Timezone != nil {
		cmd["timezone"] = *schedule.Timezone
	}
	if schedule.MaxExecutions != nil {
		cmd["max_executions"] = *schedule.MaxExecutions
	}
	_, err := tc.send(cmd)
	return err
}

// ListSchedules lists all schedules via TCP.
func (tc *TcpClient) ListSchedules() ([]Schedule, error) {
	resp, err := tc.send(map[string]interface{}{
		"cmd": "schedule_list",
	})
	if err != nil {
		return nil, err
	}
	if raw, ok := resp["schedules"]; ok {
		var schedules []Schedule
		if err := json.Unmarshal(raw, &schedules); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse schedules from response"}
		}
		return schedules, nil
	}
	return []Schedule{}, nil
}

// GetSchedule gets a specific schedule by name via TCP.
// Returns nil if the schedule is not found.
func (tc *TcpClient) GetSchedule(name string) (*Schedule, error) {
	resp, err := tc.send(map[string]interface{}{
		"cmd":  "schedule_get",
		"name": name,
	})
	if err != nil {
		if rqe, ok := IsRustQueueError(err); ok && rqe.IsNotFound() {
			return nil, nil
		}
		return nil, err
	}
	if raw, ok := resp["schedule"]; ok {
		var schedule Schedule
		if err := json.Unmarshal(raw, &schedule); err != nil {
			return nil, &RustQueueError{Code: "PARSE_ERROR", Message: "failed to parse schedule from response"}
		}
		return &schedule, nil
	}
	return nil, nil
}

// DeleteSchedule deletes a schedule via TCP.
func (tc *TcpClient) DeleteSchedule(name string) error {
	_, err := tc.send(map[string]interface{}{
		"cmd":  "schedule_delete",
		"name": name,
	})
	return err
}

// PauseSchedule pauses a schedule via TCP.
func (tc *TcpClient) PauseSchedule(name string) error {
	_, err := tc.send(map[string]interface{}{
		"cmd":  "schedule_pause",
		"name": name,
	})
	return err
}

// ResumeSchedule resumes a paused schedule via TCP.
func (tc *TcpClient) ResumeSchedule(name string) error {
	_, err := tc.send(map[string]interface{}{
		"cmd":  "schedule_resume",
		"name": name,
	})
	return err
}

// --- Internal: Connection ---

func (tc *TcpClient) establishConnection() error {
	addr := fmt.Sprintf("%s:%d", tc.host, tc.port)
	conn, err := net.DialTimeout("tcp", addr, 10*time.Second)
	if err != nil {
		return &RustQueueError{
			Code:    "CONNECTION_ERROR",
			Message: fmt.Sprintf("failed to connect to %s: %v", addr, err),
		}
	}

	tc.mu.Lock()
	tc.conn = conn
	tc.connected = true
	tc.mu.Unlock()

	tc.reconnectMu.Lock()
	tc.reconnectAttempts = 0
	tc.reconnectMu.Unlock()

	tc.done = make(chan struct{})
	go tc.readLoop()

	// Authenticate if token is configured.
	if tc.cfg.token != "" {
		if err := tc.authenticate(); err != nil {
			tc.cleanup("authentication failed")
			return err
		}
	}

	return nil
}

// readLoop reads newline-delimited JSON responses from the server and dispatches
// them to pending requests in FIFO order.
func (tc *TcpClient) readLoop() {
	defer close(tc.done)

	tc.mu.Lock()
	conn := tc.conn
	tc.mu.Unlock()

	scanner := bufio.NewScanner(conn)
	// Allow up to 1MB per line.
	scanner.Buffer(make([]byte, 64*1024), 1024*1024)

	for scanner.Scan() {
		line := scanner.Text()
		if len(line) == 0 {
			continue
		}

		tc.pendingMu.Lock()
		if len(tc.pending) == 0 {
			tc.pendingMu.Unlock()
			continue
		}
		req := tc.pending[0]
		tc.pending = tc.pending[1:]
		tc.pendingMu.Unlock()

		var raw map[string]json.RawMessage
		if err := json.Unmarshal([]byte(line), &raw); err != nil {
			req.ch <- tcpResponse{err: &RustQueueError{Code: "PARSE_ERROR", Message: fmt.Sprintf("invalid JSON response: %s", line)}}
			continue
		}

		// Check for ok=false.
		if okRaw, exists := raw["ok"]; exists {
			var ok bool
			if err := json.Unmarshal(okRaw, &ok); err == nil && !ok {
				if errRaw, exists := raw["error"]; exists {
					var serverErr struct {
						Code    string `json:"code"`
						Message string `json:"message"`
					}
					if err := json.Unmarshal(errRaw, &serverErr); err == nil {
						req.ch <- tcpResponse{err: &RustQueueError{Code: serverErr.Code, Message: serverErr.Message}}
						continue
					}
				}
				req.ch <- tcpResponse{err: &RustQueueError{Code: "UNKNOWN_ERROR", Message: "server returned ok=false without error details"}}
				continue
			}
		}

		req.ch <- tcpResponse{data: raw}
	}

	// Connection closed or error.
	tc.mu.Lock()
	wasClosing := tc.closing
	tc.mu.Unlock()

	if !wasClosing {
		reason := "connection closed by server"
		if err := scanner.Err(); err != nil {
			reason = fmt.Sprintf("socket error: %v", err)
		}
		tc.cleanup(reason)
		tc.maybeReconnect()
	}
}

func (tc *TcpClient) authenticate() error {
	resp, err := tc.send(map[string]interface{}{
		"cmd":   "auth",
		"token": tc.cfg.token,
	})
	if err != nil {
		return err
	}
	if okRaw, exists := resp["ok"]; exists {
		var ok bool
		if err := json.Unmarshal(okRaw, &ok); err == nil && !ok {
			return &RustQueueError{Code: "UNAUTHORIZED", Message: "authentication failed"}
		}
	}
	return nil
}

// send serializes a command to JSON, writes it to the TCP connection, and waits
// for the response. Writes are serialized via mutex to support concurrent callers.
func (tc *TcpClient) send(command map[string]interface{}) (map[string]json.RawMessage, error) {
	tc.mu.Lock()
	if !tc.connected || tc.conn == nil {
		tc.mu.Unlock()
		return nil, &RustQueueError{Code: "CONNECTION_ERROR", Message: "not connected; call Connect() first"}
	}
	conn := tc.conn
	tc.mu.Unlock()

	data, err := json.Marshal(command)
	if err != nil {
		return nil, &RustQueueError{Code: "MARSHAL_ERROR", Message: fmt.Sprintf("failed to marshal command: %v", err)}
	}
	data = append(data, '\n')

	req := &pendingRequest{ch: make(chan tcpResponse, 1)}

	tc.pendingMu.Lock()
	tc.pending = append(tc.pending, req)
	tc.pendingMu.Unlock()

	tc.mu.Lock()
	_, writeErr := conn.Write(data)
	tc.mu.Unlock()

	if writeErr != nil {
		// Remove the pending request.
		tc.pendingMu.Lock()
		for i, p := range tc.pending {
			if p == req {
				tc.pending = append(tc.pending[:i], tc.pending[i+1:]...)
				break
			}
		}
		tc.pendingMu.Unlock()
		return nil, &RustQueueError{Code: "WRITE_ERROR", Message: fmt.Sprintf("failed to write to socket: %v", writeErr)}
	}

	// Wait for the response.
	resp := <-req.ch
	if resp.err != nil {
		return nil, resp.err
	}
	return resp.data, nil
}

func (tc *TcpClient) cleanup(reason string) {
	tc.mu.Lock()
	tc.connected = false
	conn := tc.conn
	tc.conn = nil
	tc.mu.Unlock()

	if conn != nil {
		conn.Close()
	}

	// Reject all pending requests.
	tc.pendingMu.Lock()
	pendingCopy := tc.pending
	tc.pending = nil
	tc.pendingMu.Unlock()

	for _, req := range pendingCopy {
		req.ch <- tcpResponse{err: &RustQueueError{Code: "CONNECTION_LOST", Message: reason}}
	}
}

func (tc *TcpClient) maybeReconnect() {
	tc.mu.Lock()
	isClosing := tc.closing
	tc.mu.Unlock()

	if isClosing || !tc.cfg.autoReconnect {
		return
	}

	tc.reconnectMu.Lock()
	if tc.reconnectAttempts >= tc.cfg.maxReconnectAttempts {
		tc.reconnectMu.Unlock()
		return
	}
	attempt := tc.reconnectAttempts
	tc.reconnectAttempts++
	tc.reconnectMu.Unlock()

	delay := time.Duration(float64(tc.cfg.reconnectDelayMs)*math.Pow(2, float64(attempt))) * time.Millisecond

	time.AfterFunc(delay, func() {
		if err := tc.establishConnection(); err != nil {
			// readLoop will call maybeReconnect again on failure.
			_ = err
		}
	})
}
