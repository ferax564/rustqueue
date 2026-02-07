// Example: HTTP producer that pushes jobs and checks queue stats.
//
// Usage:
//
//	go run ./examples/http_producer/main.go
package main

import (
	"context"
	"fmt"
	"log"

	"github.com/rustqueue/rustqueue-go/rustqueue"
)

func main() {
	// Create an HTTP client. Use rustqueue.WithToken("secret") for auth.
	client := rustqueue.NewClient("http://localhost:6790")

	ctx := context.Background()

	// Check server health.
	health, err := client.Health(ctx)
	if err != nil {
		log.Fatalf("health check failed: %v", err)
	}
	fmt.Printf("Server healthy: %s (uptime: %.0fs)\n", health.Version, health.UptimeSeconds)

	// Push a single job.
	jobID, err := client.Push(ctx, "emails", "send-welcome", map[string]string{
		"to":       "alice@example.com",
		"template": "welcome",
	}, nil)
	if err != nil {
		log.Fatalf("push failed: %v", err)
	}
	fmt.Printf("Pushed job: %s\n", jobID)

	// Push with options.
	priority := 10
	maxAttempts := 5
	jobID2, err := client.Push(ctx, "emails", "send-receipt", map[string]string{
		"to":       "bob@example.com",
		"order_id": "12345",
	}, &rustqueue.JobOptions{
		Priority:    &priority,
		MaxAttempts: &maxAttempts,
	})
	if err != nil {
		log.Fatalf("push with options failed: %v", err)
	}
	fmt.Printf("Pushed priority job: %s\n", jobID2)

	// Push a batch.
	ids, err := client.PushBatch(ctx, "emails", []rustqueue.PushJobInput{
		{Name: "send-welcome", Data: map[string]string{"to": "carol@example.com"}},
		{Name: "send-welcome", Data: map[string]string{"to": "dave@example.com"}},
	})
	if err != nil {
		log.Fatalf("push batch failed: %v", err)
	}
	fmt.Printf("Pushed batch: %v\n", ids)

	// Check queue stats.
	stats, err := client.GetQueueStats(ctx, "emails")
	if err != nil {
		log.Fatalf("get stats failed: %v", err)
	}
	fmt.Printf("Queue stats — waiting: %d, active: %d, completed: %d\n",
		stats.Waiting, stats.Active, stats.Completed)

	// List all queues.
	queues, err := client.ListQueues(ctx)
	if err != nil {
		log.Fatalf("list queues failed: %v", err)
	}
	for _, q := range queues {
		fmt.Printf("Queue '%s': %d waiting, %d active\n", q.Name, q.Counts.Waiting, q.Counts.Active)
	}
}
