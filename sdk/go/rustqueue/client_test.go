package rustqueue

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

// newTestServer creates an httptest.Server that returns the given JSON body
// for every request. The caller can inspect received requests via the returned
// channel.
func newTestServer(t *testing.T, statusCode int, body interface{}) (*httptest.Server, chan *http.Request) {
	t.Helper()
	reqCh := make(chan *http.Request, 16)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reqCh <- r
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(statusCode)
		json.NewEncoder(w).Encode(body)
	}))
	t.Cleanup(server.Close)
	return server, reqCh
}

func TestPush(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"id": "test-job-id-123",
	}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	id, err := client.Push(context.Background(), "emails", "send-welcome", map[string]string{"to": "alice@example.com"}, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "test-job-id-123" {
		t.Fatalf("expected id 'test-job-id-123', got '%s'", id)
	}

	req := <-reqs
	if req.Method != http.MethodPost {
		t.Fatalf("expected POST, got %s", req.Method)
	}
	if req.URL.Path != "/api/v1/queues/emails/jobs" {
		t.Fatalf("expected path '/api/v1/queues/emails/jobs', got '%s'", req.URL.Path)
	}
}

func TestPushWithOptions(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"id": "opt-job-id",
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	priority := 10
	maxAttempts := 5
	opts := &JobOptions{
		Priority:    &priority,
		MaxAttempts: &maxAttempts,
	}
	id, err := client.Push(context.Background(), "emails", "send", nil, opts)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if id != "opt-job-id" {
		t.Fatalf("expected id 'opt-job-id', got '%s'", id)
	}
}

func TestPushBatch(t *testing.T) {
	resp := map[string]interface{}{
		"ok":  true,
		"ids": []string{"id-1", "id-2"},
	}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	ids, err := client.PushBatch(context.Background(), "emails", []PushJobInput{
		{Name: "send-welcome", Data: map[string]string{"to": "alice@example.com"}},
		{Name: "send-welcome", Data: map[string]string{"to": "bob@example.com"}},
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(ids) != 2 {
		t.Fatalf("expected 2 ids, got %d", len(ids))
	}
	if ids[0] != "id-1" || ids[1] != "id-2" {
		t.Fatalf("unexpected ids: %v", ids)
	}

	req := <-reqs
	if req.Method != http.MethodPost {
		t.Fatalf("expected POST, got %s", req.Method)
	}
}

func TestPullSingle(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"job": map[string]interface{}{
			"id":                "job-1",
			"name":              "send-welcome",
			"queue":             "emails",
			"state":             "active",
			"data":              nil,
			"priority":          0,
			"created_at":        "2026-01-01T00:00:00Z",
			"updated_at":        "2026-01-01T00:00:00Z",
			"max_attempts":      3,
			"attempt":           1,
			"backoff":           "exponential",
			"backoff_delay_ms":  1000,
			"lifo":              false,
			"remove_on_complete": false,
			"remove_on_fail":    false,
			"logs":              []interface{}{},
			"tags":              []interface{}{},
			"depends_on":        []interface{}{},
		},
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	jobs, err := client.Pull(context.Background(), "emails", 1)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(jobs) != 1 {
		t.Fatalf("expected 1 job, got %d", len(jobs))
	}
	if jobs[0].ID != "job-1" {
		t.Fatalf("expected job id 'job-1', got '%s'", jobs[0].ID)
	}
}

func TestPullEmpty(t *testing.T) {
	resp := map[string]interface{}{
		"ok":  true,
		"job": nil,
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	jobs, err := client.Pull(context.Background(), "emails", 1)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(jobs) != 0 {
		t.Fatalf("expected 0 jobs, got %d", len(jobs))
	}
}

func TestAck(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.Ack(context.Background(), "job-1", map[string]string{"sent": "true"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/jobs/job-1/ack" {
		t.Fatalf("expected path '/api/v1/jobs/job-1/ack', got '%s'", req.URL.Path)
	}
}

func TestFail(t *testing.T) {
	resp := map[string]interface{}{
		"ok":              true,
		"retry":           true,
		"next_attempt_at": "2026-01-01T00:01:00Z",
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	result, err := client.Fail(context.Background(), "job-1", "something went wrong")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !result.Retry {
		t.Fatalf("expected retry=true")
	}
	if result.NextAttemptAt == nil {
		t.Fatalf("expected next_attempt_at to be non-nil")
	}
}

func TestListQueues(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"queues": []map[string]interface{}{
			{
				"name": "emails",
				"counts": map[string]interface{}{
					"waiting":   5,
					"active":    2,
					"delayed":   1,
					"completed": 100,
					"failed":    3,
					"dlq":       0,
				},
			},
		},
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	queues, err := client.ListQueues(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(queues) != 1 {
		t.Fatalf("expected 1 queue, got %d", len(queues))
	}
	if queues[0].Name != "emails" {
		t.Fatalf("expected queue name 'emails', got '%s'", queues[0].Name)
	}
	if queues[0].Counts.Waiting != 5 {
		t.Fatalf("expected waiting=5, got %d", queues[0].Counts.Waiting)
	}
}

func TestHealth(t *testing.T) {
	resp := map[string]interface{}{
		"ok":             true,
		"status":         "healthy",
		"version":        "0.11.0",
		"uptime_seconds": 42.5,
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	health, err := client.Health(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !health.OK {
		t.Fatalf("expected ok=true")
	}
	if health.Version != "0.11.0" {
		t.Fatalf("expected version '0.11.0', got '%s'", health.Version)
	}
}

func TestServerError(t *testing.T) {
	resp := map[string]interface{}{
		"ok": false,
		"error": map[string]interface{}{
			"code":    "NOT_FOUND",
			"message": "job not found",
		},
	}
	server, _ := newTestServer(t, 404, resp)
	client := NewClient(server.URL)

	_, err := client.Push(context.Background(), "emails", "send", nil, nil)
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	rqe, ok := IsRustQueueError(err)
	if !ok {
		t.Fatalf("expected RustQueueError, got %T", err)
	}
	if rqe.Code != "NOT_FOUND" {
		t.Fatalf("expected code 'NOT_FOUND', got '%s'", rqe.Code)
	}
	if rqe.StatusCode != 404 {
		t.Fatalf("expected status 404, got %d", rqe.StatusCode)
	}
}

func TestGetJobNotFound(t *testing.T) {
	resp := map[string]interface{}{
		"ok": false,
		"error": map[string]interface{}{
			"code":    "NOT_FOUND",
			"message": "job not found",
		},
	}
	server, _ := newTestServer(t, 404, resp)
	client := NewClient(server.URL)

	job, err := client.GetJob(context.Background(), "nonexistent")
	if err != nil {
		t.Fatalf("GetJob should return nil for not found, got error: %v", err)
	}
	if job != nil {
		t.Fatalf("expected nil job, got %+v", job)
	}
}

func TestAuthToken(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL, WithToken("my-secret-token"))

	_, err := client.Health(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	auth := req.Header.Get("Authorization")
	if auth != "Bearer my-secret-token" {
		t.Fatalf("expected 'Bearer my-secret-token', got '%s'", auth)
	}
}

func TestCancel(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.Cancel(context.Background(), "job-123")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/jobs/job-123/cancel" {
		t.Fatalf("expected path '/api/v1/jobs/job-123/cancel', got '%s'", req.URL.Path)
	}
}

func TestProgress(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.Progress(context.Background(), "job-123", 50, "halfway done")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/jobs/job-123/progress" {
		t.Fatalf("expected path '/api/v1/jobs/job-123/progress', got '%s'", req.URL.Path)
	}
}

func TestHeartbeat(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.Heartbeat(context.Background(), "job-123")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/jobs/job-123/heartbeat" {
		t.Fatalf("expected path '/api/v1/jobs/job-123/heartbeat', got '%s'", req.URL.Path)
	}
}

func TestGetQueueStats(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"counts": map[string]interface{}{
			"waiting":   10,
			"active":    3,
			"delayed":   0,
			"completed": 500,
			"failed":    7,
			"dlq":       2,
		},
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	counts, err := client.GetQueueStats(context.Background(), "emails")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if counts.Waiting != 10 {
		t.Fatalf("expected waiting=10, got %d", counts.Waiting)
	}
	if counts.Dlq != 2 {
		t.Fatalf("expected dlq=2, got %d", counts.Dlq)
	}
}

func TestCreateSchedule(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	cron := "0 * * * *"
	err := client.CreateSchedule(context.Background(), ScheduleInput{
		Name:     "hourly-cleanup",
		Queue:    "maintenance",
		JobName:  "cleanup-expired",
		CronExpr: &cron,
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/schedules" {
		t.Fatalf("expected path '/api/v1/schedules', got '%s'", req.URL.Path)
	}
	if req.Method != http.MethodPost {
		t.Fatalf("expected POST, got %s", req.Method)
	}
}

func TestDeleteSchedule(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.DeleteSchedule(context.Background(), "hourly-cleanup")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/schedules/hourly-cleanup" {
		t.Fatalf("expected path '/api/v1/schedules/hourly-cleanup', got '%s'", req.URL.Path)
	}
	if req.Method != http.MethodDelete {
		t.Fatalf("expected DELETE, got %s", req.Method)
	}
}

func TestPauseResumeQueue(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.PauseQueue(context.Background(), "emails")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	req := <-reqs
	if req.URL.Path != "/api/v1/queues/emails/pause" {
		t.Fatalf("expected path '/api/v1/queues/emails/pause', got '%s'", req.URL.Path)
	}

	err = client.ResumeQueue(context.Background(), "emails")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	req = <-reqs
	if req.URL.Path != "/api/v1/queues/emails/resume" {
		t.Fatalf("expected path '/api/v1/queues/emails/resume', got '%s'", req.URL.Path)
	}
}

func TestPauseResumeSchedule(t *testing.T) {
	resp := map[string]interface{}{"ok": true}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	err := client.PauseSchedule(context.Background(), "hourly-cleanup")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	req := <-reqs
	if req.URL.Path != "/api/v1/schedules/hourly-cleanup/pause" {
		t.Fatalf("expected path '/api/v1/schedules/hourly-cleanup/pause', got '%s'", req.URL.Path)
	}

	err = client.ResumeSchedule(context.Background(), "hourly-cleanup")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	req = <-reqs
	if req.URL.Path != "/api/v1/schedules/hourly-cleanup/resume" {
		t.Fatalf("expected path '/api/v1/schedules/hourly-cleanup/resume', got '%s'", req.URL.Path)
	}
}

func TestGetDlqJobs(t *testing.T) {
	resp := map[string]interface{}{
		"ok":   true,
		"jobs": []interface{}{},
	}
	server, reqs := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	jobs, err := client.GetDlqJobs(context.Background(), "emails", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(jobs) != 0 {
		t.Fatalf("expected 0 jobs, got %d", len(jobs))
	}

	req := <-reqs
	if req.URL.Path != "/api/v1/queues/emails/dlq" {
		t.Fatalf("expected path '/api/v1/queues/emails/dlq', got '%s'", req.URL.Path)
	}
	if req.URL.Query().Get("limit") != "10" {
		t.Fatalf("expected limit=10, got '%s'", req.URL.Query().Get("limit"))
	}
}

func TestErrorInterface(t *testing.T) {
	err := &RustQueueError{Code: "TEST", Message: "test message", StatusCode: 500}
	if err.Error() != "RustQueueError [TEST] (HTTP 500): test message" {
		t.Fatalf("unexpected error string: %s", err.Error())
	}

	err2 := &RustQueueError{Code: "TEST", Message: "test message"}
	if err2.Error() != "RustQueueError [TEST]: test message" {
		t.Fatalf("unexpected error string: %s", err2.Error())
	}
}

func TestBuildPushBody(t *testing.T) {
	priority := 5
	opts := &JobOptions{Priority: &priority}
	body := buildPushBody("test-job", map[string]string{"key": "value"}, opts)

	if body["name"] != "test-job" {
		t.Fatalf("expected name 'test-job', got '%v'", body["name"])
	}

	// Priority should be flattened into the body at the top level.
	p, ok := body["priority"]
	if !ok {
		t.Fatal("expected priority in body")
	}
	// json.Number from the marshal/unmarshal cycle will produce a float64.
	if pf, ok := p.(float64); ok {
		if pf != 5 {
			t.Fatalf("expected priority=5, got %v", pf)
		}
	} else {
		t.Fatalf("expected float64 priority, got %T", p)
	}
}

func TestListSchedules(t *testing.T) {
	resp := map[string]interface{}{
		"ok": true,
		"schedules": []map[string]interface{}{
			{
				"name":            "hourly-cleanup",
				"queue":           "maintenance",
				"job_name":        "cleanup-expired",
				"job_data":        nil,
				"job_options":     nil,
				"cron_expr":       "0 * * * *",
				"every_ms":        nil,
				"timezone":        nil,
				"max_executions":  nil,
				"execution_count": 42,
				"paused":          false,
				"last_run_at":     "2026-01-01T00:00:00Z",
				"next_run_at":     "2026-01-01T01:00:00Z",
				"created_at":      "2025-12-01T00:00:00Z",
				"updated_at":      "2026-01-01T00:00:00Z",
			},
		},
	}
	server, _ := newTestServer(t, 200, resp)
	client := NewClient(server.URL)

	schedules, err := client.ListSchedules(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(schedules) != 1 {
		t.Fatalf("expected 1 schedule, got %d", len(schedules))
	}
	if schedules[0].Name != "hourly-cleanup" {
		t.Fatalf("expected name 'hourly-cleanup', got '%s'", schedules[0].Name)
	}
	if schedules[0].ExecutionCount != 42 {
		t.Fatalf("expected execution_count=42, got %d", schedules[0].ExecutionCount)
	}
}
