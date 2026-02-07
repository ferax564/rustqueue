"""Basic worker example for the RustQueue Python SDK.

This script demonstrates the core push / pull / ack lifecycle:

1. Push a job to a queue.
2. Pull the job from the queue.
3. Process the job (simulated).
4. Acknowledge completion.

Usage:
    # Start a RustQueue server first:
    #   rustqueue serve
    #
    # Then run this example:
    python basic_worker.py
"""

from __future__ import annotations

import time

from rustqueue import RustQueueClient, RustQueueError


def main() -> None:
    # Connect to a local RustQueue server (default http://localhost:6790).
    # Pass token="your-secret" if authentication is enabled.
    client = RustQueueClient()

    # -- Health check --------------------------------------------------------
    health = client.health()
    print(f"Server is {health['status']} (v{health['version']}, uptime {health['uptime_seconds']}s)")

    # -- Push a job ----------------------------------------------------------
    queue = "demo"
    job_id = client.push(
        queue,
        "send-email",
        {"to": "user@example.com", "subject": "Hello from RustQueue!"},
        max_attempts=3,
        timeout_ms=30_000,
    )
    print(f"Pushed job {job_id} to queue '{queue}'")

    # -- Pull the job --------------------------------------------------------
    jobs = client.pull(queue)
    if not jobs:
        print("No jobs available (unexpected!)")
        return

    job = jobs[0]
    print(f"Pulled job {job['id']} (name={job['name']})")

    # -- Process the job (simulate work) -------------------------------------
    for pct in (25, 50, 75, 100):
        time.sleep(0.1)
        client.progress(job["id"], pct, message=f"Step {pct}%")
        print(f"  progress: {pct}%")

    # -- Acknowledge completion ----------------------------------------------
    client.ack(job["id"], result={"delivered": True})
    print(f"Acked job {job['id']}")

    # -- Verify the job is completed -----------------------------------------
    completed = client.get_job(job_id)
    if completed:
        print(f"Job state: {completed.get('state')}")

    # -- Push and fail a job (demonstrate retries) ---------------------------
    fail_id = client.push(queue, "might-fail", {"attempt": 1}, max_attempts=2)
    pulled = client.pull(queue)
    if pulled:
        result = client.fail(pulled[0]["id"], "simulated error")
        print(f"Failed job {pulled[0]['id']}: retry={result['retry']}")

    # -- Queue stats ---------------------------------------------------------
    stats = client.get_queue_stats(queue)
    print(f"Queue '{queue}' stats: {stats}")

    # -- Batch push ----------------------------------------------------------
    ids = client.push_batch(queue, [
        {"name": "batch-job", "data": {"index": 0}},
        {"name": "batch-job", "data": {"index": 1}},
        {"name": "batch-job", "data": {"index": 2}},
    ])
    print(f"Batch pushed {len(ids)} jobs: {ids}")

    print("\nDone!")


if __name__ == "__main__":
    try:
        main()
    except RustQueueError as exc:
        print(f"RustQueue error: {exc}")
    except ConnectionError as exc:
        print(f"Could not connect to RustQueue server: {exc}")
