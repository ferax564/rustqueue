#!/usr/bin/env python3
"""Sustained load (soak) test for RustQueue.

Spawns a RustQueue server with a configurable backend, then drives push+pull+ack
cycles at a target rate from N worker threads. Samples throughput, latency
percentiles, memory (RSS), CPU, queue depth, and error rate every few seconds.

Outputs a CSV time-series to stdout (or a file) and prints a console summary
at the end with pass/fail assertions.

Requirements:
    - Python >= 3.8
    - psutil (pip install psutil) — used for RSS/CPU sampling
    - A built RustQueue release binary (cargo build --release)

Usage:
    python scripts/soak_test.py --duration 5m --rate 500 --backend hybrid --concurrency 10
    python scripts/soak_test.py --duration 30s --rate 100 --backend redb --verbose
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import os
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Deque, List, Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
RUSTQUEUE_BIN = REPO_ROOT / "target" / "release" / "rustqueue"
DEFAULT_HTTP_PORT = 26790
DEFAULT_TCP_PORT = 26789


# ---------------------------------------------------------------------------
# Duration parsing
# ---------------------------------------------------------------------------

def parse_duration(s: str) -> float:
    """Parse a human-friendly duration string (e.g. '5m', '30s', '2h') to seconds."""
    s = s.strip().lower()
    if s.endswith("h"):
        return float(s[:-1]) * 3600
    if s.endswith("m"):
        return float(s[:-1]) * 60
    if s.endswith("s"):
        return float(s[:-1])
    return float(s)


# ---------------------------------------------------------------------------
# HTTP helpers (stdlib only)
# ---------------------------------------------------------------------------

def http_post(url: str, body: dict, timeout: float = 10.0) -> dict:
    """POST JSON and return parsed response body."""
    data = json.dumps(body, separators=(",", ":")).encode("utf-8")
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def http_get(url: str, timeout: float = 10.0) -> dict:
    """GET and return parsed response body."""
    req = urllib.request.Request(url, method="GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


# ---------------------------------------------------------------------------
# Server management
# ---------------------------------------------------------------------------

def generate_config(backend: str, data_dir: Path) -> str:
    """Generate a rustqueue.toml config string for the given backend."""
    storage_lines = [
        "[storage]",
        f'backend = "{backend}"',
        f'path = "{data_dir.as_posix()}"',
        'redb_durability = "immediate"',
    ]
    if backend == "hybrid":
        storage_lines.extend([
            "hybrid_snapshot_interval_ms = 1000",
            "hybrid_max_dirty = 5000",
        ])
    if backend in ("redb", "hybrid"):
        storage_lines.extend([
            "write_coalescing_enabled = false",
        ])

    return "\n".join([
        "[server]",
        'host = "127.0.0.1"',
        f"http_port = {DEFAULT_HTTP_PORT}",
        f"tcp_port = {DEFAULT_TCP_PORT}",
        "",
        *storage_lines,
        "",
        "[auth]",
        "enabled = false",
        "tokens = []",
    ])


def start_server(backend: str) -> tuple:
    """Build (if needed) and start a RustQueue server. Returns (Popen, tmpdir_path)."""
    if not RUSTQUEUE_BIN.exists():
        print("Building release binary ...", file=sys.stderr)
        subprocess.run(
            ["cargo", "build", "--release"],
            cwd=REPO_ROOT,
            check=True,
            capture_output=True,
        )

    tmpdir = Path(tempfile.mkdtemp(prefix="rustqueue-soak-"))
    data_dir = tmpdir / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    cfg_path = tmpdir / "rustqueue.toml"
    cfg_path.write_text(generate_config(backend, data_dir), encoding="utf-8")

    proc = subprocess.Popen(
        [
            str(RUSTQUEUE_BIN),
            "serve",
            "--config",
            str(cfg_path),
            "--http-port",
            str(DEFAULT_HTTP_PORT),
            "--tcp-port",
            str(DEFAULT_TCP_PORT),
        ],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env={**os.environ, "RUST_LOG": "warn"},
    )

    # Wait for health endpoint
    health_url = f"http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v1/health"
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            http_get(health_url, timeout=1)
            return proc, tmpdir
        except Exception:
            if proc.poll() is not None:
                out = proc.stdout.read() if proc.stdout else ""
                raise RuntimeError(f"Server exited early:\n{out}")
            time.sleep(0.2)

    out = ""
    if proc.stdout:
        try:
            out = proc.stdout.read()
        except Exception:
            pass
    proc.kill()
    raise RuntimeError(f"Server did not become ready:\n{out}")


def stop_server(proc: subprocess.Popen, tmpdir: Path) -> None:
    """Terminate the server and clean up temp files."""
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    # Best-effort cleanup
    try:
        for p in sorted(tmpdir.rglob("*"), reverse=True):
            if p.is_file() or p.is_symlink():
                p.unlink(missing_ok=True)
            elif p.is_dir():
                p.rmdir()
        tmpdir.rmdir()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Sampling data
# ---------------------------------------------------------------------------

@dataclass
class Sample:
    timestamp: float
    elapsed_s: float
    throughput: float  # ops/sec in this interval
    latency_p50_ms: float
    latency_p95_ms: float
    latency_p99_ms: float
    rss_mb: float
    cpu_percent: float
    queue_depth: int
    errors: int


@dataclass
class WorkerStats:
    """Thread-safe stats accumulator for worker threads."""
    lock: threading.Lock = field(default_factory=threading.Lock)
    latencies: Deque[float] = field(default_factory=deque)  # seconds
    ops: int = 0
    errors: int = 0

    def record(self, latency_s: float) -> None:
        with self.lock:
            self.latencies.append(latency_s)
            self.ops += 1

    def record_error(self) -> None:
        with self.lock:
            self.errors += 1

    def drain(self) -> tuple:
        """Drain accumulated stats. Returns (ops, errors, latencies_list)."""
        with self.lock:
            ops = self.ops
            errors = self.errors
            lats = list(self.latencies)
            self.ops = 0
            self.errors = 0
            self.latencies.clear()
            return ops, errors, lats


# ---------------------------------------------------------------------------
# Worker loop
# ---------------------------------------------------------------------------

def worker_loop(
    worker_id: int,
    stats: WorkerStats,
    stop_event: threading.Event,
    rate_per_worker: float,
    base_url: str,
    verbose: bool,
) -> None:
    """Push + pull + ack cycle in a tight loop at the target rate."""
    queue = f"soak-{worker_id}"
    push_url = f"{base_url}/api/v1/queues/{queue}/jobs"
    pull_url = f"{base_url}/api/v1/queues/{queue}/jobs?count=1"
    counter = 0
    interval = 1.0 / rate_per_worker if rate_per_worker > 0 else 0

    while not stop_event.is_set():
        cycle_start = time.monotonic()
        try:
            counter += 1
            name = f"soak-{worker_id}-{counter}"

            # Push
            t0 = time.perf_counter()
            push_resp = http_post(push_url, {"name": name, "data": {"i": counter}})
            job_id = push_resp.get("id")
            if not job_id:
                stats.record_error()
                if verbose:
                    print(f"[worker-{worker_id}] push returned no id: {push_resp}", file=sys.stderr)
                continue

            # Pull
            pull_resp = http_get(pull_url)
            job = pull_resp.get("job")
            if not job:
                stats.record_error()
                if verbose:
                    print(f"[worker-{worker_id}] pull returned no job: {pull_resp}", file=sys.stderr)
                continue

            # Ack
            ack_url = f"{base_url}/api/v1/jobs/{job['id']}/ack"
            http_post(ack_url, {})

            elapsed = time.perf_counter() - t0
            stats.record(elapsed)

        except Exception as exc:
            stats.record_error()
            if verbose:
                print(f"[worker-{worker_id}] error: {exc}", file=sys.stderr)

        # Rate-limit: sleep for remainder of interval
        if interval > 0:
            spent = time.monotonic() - cycle_start
            remaining = interval - spent
            if remaining > 0:
                time.sleep(remaining)


# ---------------------------------------------------------------------------
# Sampling loop
# ---------------------------------------------------------------------------

def get_rss_cpu(pid: int) -> tuple:
    """Return (rss_mb, cpu_percent) using psutil. Falls back to (0, 0) if unavailable."""
    try:
        import psutil
        proc = psutil.Process(pid)
        rss_bytes = proc.memory_info().rss
        cpu = proc.cpu_percent(interval=0.1)
        return rss_bytes / (1024 * 1024), cpu
    except Exception:
        return 0.0, 0.0


def get_queue_depth(base_url: str) -> int:
    """Sum waiting jobs across all queues."""
    try:
        queues = http_get(f"{base_url}/api/v1/queues")
        total = 0
        for q in queues.get("queues", []):
            stats_resp = http_get(f"{base_url}/api/v1/queues/{q['name']}/stats")
            total += stats_resp.get("waiting", 0)
        return total
    except Exception:
        return -1


def percentile(data: List[float], pct: float) -> float:
    """Calculate percentile from sorted data."""
    if not data:
        return 0.0
    sorted_data = sorted(data)
    k = (len(sorted_data) - 1) * (pct / 100.0)
    f = int(k)
    c = f + 1
    if c >= len(sorted_data):
        return sorted_data[f]
    return sorted_data[f] + (k - f) * (sorted_data[c] - sorted_data[f])


def run_sampling_loop(
    stats: WorkerStats,
    server_pid: int,
    base_url: str,
    stop_event: threading.Event,
    sample_interval: float,
    start_time: float,
) -> List[Sample]:
    """Periodically drain worker stats and record samples."""
    samples: List[Sample] = []
    while not stop_event.is_set():
        time.sleep(sample_interval)
        now = time.time()
        elapsed = now - start_time
        ops, errors, lats = stats.drain()

        throughput = ops / sample_interval if sample_interval > 0 else 0

        p50 = percentile(lats, 50) * 1000  # ms
        p95 = percentile(lats, 95) * 1000
        p99 = percentile(lats, 99) * 1000

        rss_mb, cpu_pct = get_rss_cpu(server_pid)
        depth = get_queue_depth(base_url)

        sample = Sample(
            timestamp=now,
            elapsed_s=round(elapsed, 1),
            throughput=round(throughput, 1),
            latency_p50_ms=round(p50, 2),
            latency_p95_ms=round(p95, 2),
            latency_p99_ms=round(p99, 2),
            rss_mb=round(rss_mb, 1),
            cpu_percent=round(cpu_pct, 1),
            queue_depth=depth,
            errors=errors,
        )
        samples.append(sample)
    return samples


# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

def write_csv(samples: List[Sample], output: io.TextIOBase) -> None:
    """Write samples as CSV."""
    writer = csv.writer(output)
    writer.writerow([
        "elapsed_s", "throughput_ops_s", "p50_ms", "p95_ms", "p99_ms",
        "rss_mb", "cpu_pct", "queue_depth", "errors",
    ])
    for s in samples:
        writer.writerow([
            s.elapsed_s, s.throughput, s.latency_p50_ms, s.latency_p95_ms,
            s.latency_p99_ms, s.rss_mb, s.cpu_percent, s.queue_depth, s.errors,
        ])


def print_summary(
    samples: List[Sample],
    duration_s: float,
    backend: str,
    concurrency: int,
    target_rate: int,
) -> dict:
    """Print console summary and return assertion results."""
    if not samples:
        print("No samples collected!")
        return {"pass": False, "reason": "no samples"}

    total_throughput = [s.throughput for s in samples]
    all_p99 = [s.latency_p99_ms for s in samples]
    all_rss = [s.rss_mb for s in samples if s.rss_mb > 0]
    total_errors = sum(s.errors for s in samples)
    total_ops = sum(s.throughput * 5 for s in samples)  # approximate

    avg_throughput = statistics.mean(total_throughput) if total_throughput else 0
    max_p99 = max(all_p99) if all_p99 else 0

    rss_start = all_rss[0] if all_rss else 0
    rss_end = all_rss[-1] if all_rss else 0
    rss_growth = rss_end - rss_start
    # Extrapolate to per-hour growth
    hours = duration_s / 3600 if duration_s > 0 else 1
    rss_growth_per_hour = rss_growth / hours if hours > 0 else rss_growth

    error_rate = (total_errors / total_ops * 100) if total_ops > 0 else 0

    # P99 stability: check if the last 20% of samples have P99 within 2x of median
    if len(all_p99) >= 5:
        median_p99 = statistics.median(all_p99)
        tail_p99 = all_p99[int(len(all_p99) * 0.8):]
        p99_stable = all(p <= median_p99 * 3 for p in tail_p99)
    else:
        p99_stable = True

    print()
    print("=" * 70)
    print(f"  SOAK TEST SUMMARY — backend={backend}, concurrency={concurrency}")
    print(f"  Target rate: {target_rate} ops/s, Duration: {duration_s:.0f}s")
    print("=" * 70)
    print(f"  Avg throughput:       {avg_throughput:>10.1f} ops/s")
    print(f"  Max P99 latency:      {max_p99:>10.2f} ms")
    print(f"  RSS start:            {rss_start:>10.1f} MB")
    print(f"  RSS end:              {rss_end:>10.1f} MB")
    print(f"  RSS growth:           {rss_growth:>10.1f} MB ({rss_growth_per_hour:.1f} MB/hour)")
    print(f"  Total errors:         {total_errors:>10d}")
    print(f"  Error rate:           {error_rate:>10.3f}%")
    print(f"  P99 stable (tail):    {'YES' if p99_stable else 'NO'}")
    print("=" * 70)

    # Assertions
    failures = []
    if rss_growth_per_hour > 50:
        failures.append(f"RSS growth {rss_growth_per_hour:.1f} MB/hour > 50 MB/hour threshold")
    if error_rate > 0.1:
        failures.append(f"Error rate {error_rate:.3f}% > 0.1% threshold")
    if not p99_stable:
        failures.append("P99 latency not stable in tail samples (>3x median)")

    if failures:
        print("  RESULT: FAIL")
        for f in failures:
            print(f"    - {f}")
        print("=" * 70)
        return {"pass": False, "failures": failures}
    else:
        print("  RESULT: PASS")
        print("=" * 70)
        return {"pass": True, "failures": []}


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(
        description="RustQueue sustained load (soak) test",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--duration", type=str, default="5m",
        help="Test duration (e.g. 30s, 5m, 1h). Default: 5m",
    )
    parser.add_argument(
        "--rate", type=int, default=500,
        help="Target operations per second (total across all workers). Default: 500",
    )
    parser.add_argument(
        "--backend", type=str, choices=["redb", "hybrid", "memory"], default="redb",
        help="Storage backend to use. Default: redb",
    )
    parser.add_argument(
        "--concurrency", type=int, default=10,
        help="Number of worker threads. Default: 10",
    )
    parser.add_argument(
        "--sample-interval", type=float, default=5.0,
        help="Seconds between metric samples. Default: 5",
    )
    parser.add_argument(
        "--csv", type=str, default=None,
        help="Path to write CSV time-series output (default: stdout summary only)",
    )
    parser.add_argument(
        "--verbose", action="store_true",
        help="Print per-worker errors to stderr",
    )
    args = parser.parse_args()

    duration_s = parse_duration(args.duration)
    rate = args.rate
    concurrency = args.concurrency
    backend = args.backend
    sample_interval = args.sample_interval
    verbose = args.verbose

    rate_per_worker = rate / concurrency if concurrency > 0 else rate

    print(f"Starting soak test: backend={backend}, duration={duration_s:.0f}s, "
          f"rate={rate} ops/s, concurrency={concurrency}", file=sys.stderr)

    # Start server
    proc: Optional[subprocess.Popen] = None
    tmpdir: Optional[Path] = None
    try:
        proc, tmpdir = start_server(backend)
        server_pid = proc.pid
        base_url = f"http://127.0.0.1:{DEFAULT_HTTP_PORT}"

        print(f"Server started (pid={server_pid})", file=sys.stderr)

        # Shared state
        stats = WorkerStats()
        stop_event = threading.Event()

        # Start worker threads
        workers: List[threading.Thread] = []
        for i in range(concurrency):
            t = threading.Thread(
                target=worker_loop,
                args=(i, stats, stop_event, rate_per_worker, base_url, verbose),
                daemon=True,
            )
            t.start()
            workers.append(t)

        # Start sampling in main thread (non-blocking via stop_event)
        start_time = time.time()

        # Run sampling in a background thread so we can sleep for duration
        sample_results: List[Sample] = []

        def sampling_thread_fn():
            nonlocal sample_results
            sample_results = run_sampling_loop(
                stats, server_pid, base_url, stop_event, sample_interval, start_time,
            )

        sampler = threading.Thread(target=sampling_thread_fn, daemon=True)
        sampler.start()

        # Wait for test duration
        try:
            time.sleep(duration_s)
        except KeyboardInterrupt:
            print("\nInterrupted, stopping...", file=sys.stderr)

        # Signal all threads to stop
        stop_event.set()

        # Wait for workers to finish
        for t in workers:
            t.join(timeout=5)
        sampler.join(timeout=10)

        # Output CSV
        if args.csv:
            with open(args.csv, "w", newline="", encoding="utf-8") as f:
                write_csv(sample_results, f)
            print(f"CSV written to {args.csv}", file=sys.stderr)
        else:
            buf = io.StringIO()
            write_csv(sample_results, buf)
            if verbose:
                print(buf.getvalue())

        # Summary
        result = print_summary(sample_results, duration_s, backend, concurrency, rate)
        return 0 if result["pass"] else 1

    finally:
        if proc is not None and tmpdir is not None:
            stop_server(proc, tmpdir)


if __name__ == "__main__":
    sys.exit(main())
