// Example: TCP worker that pulls and processes jobs with heartbeat.
//
// Usage:
//
//	go run ./examples/tcp_worker/main.go
package main

import (
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/rustqueue/rustqueue-go/rustqueue"
)

func main() {
	// Create a TCP client with auto-reconnect.
	client := rustqueue.NewTcpClient("127.0.0.1", 6789,
		rustqueue.WithAutoReconnect(true),
		rustqueue.WithMaxReconnectAttempts(20),
		rustqueue.WithReconnectDelay(500),
	)

	// Connect to the server.
	if err := client.Connect(); err != nil {
		log.Fatalf("failed to connect: %v", err)
	}
	defer client.Disconnect()
	fmt.Println("Connected to RustQueue TCP server")

	// Set up graceful shutdown.
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	// Worker loop: pull, process, ack.
	for {
		select {
		case <-sigCh:
			fmt.Println("\nShutting down worker...")
			return
		default:
		}

		// Pull a job.
		jobs, err := client.Pull("emails", 1)
		if err != nil {
			log.Printf("pull error: %v", err)
			time.Sleep(time.Second)
			continue
		}
		if len(jobs) == 0 {
			// No jobs available, wait before polling again.
			time.Sleep(500 * time.Millisecond)
			continue
		}

		job := jobs[0]
		fmt.Printf("Processing job %s (%s)\n", job.ID, job.Name)

		// Simulate work with heartbeat and progress.
		for i := 1; i <= 5; i++ {
			time.Sleep(200 * time.Millisecond)

			// Update progress.
			if err := client.Progress(job.ID, i*20, fmt.Sprintf("step %d/5", i)); err != nil {
				log.Printf("progress error: %v", err)
			}

			// Send heartbeat.
			if err := client.Heartbeat(job.ID); err != nil {
				log.Printf("heartbeat error: %v", err)
			}
		}

		// Acknowledge completion.
		if err := client.Ack(job.ID, map[string]string{"status": "processed"}); err != nil {
			log.Printf("ack error: %v", err)
			// Report failure instead.
			if _, failErr := client.Fail(job.ID, fmt.Sprintf("ack failed: %v", err)); failErr != nil {
				log.Printf("fail error: %v", failErr)
			}
			continue
		}

		fmt.Printf("Completed job %s\n", job.ID)
	}
}
