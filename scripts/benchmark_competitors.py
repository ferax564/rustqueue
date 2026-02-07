#!/usr/bin/env python3
"""Run local RustQueue vs competitor benchmarks and write a markdown report.

Benchmarks:
- produce: enqueue N messages
- consume: dequeue(+ack where relevant) N pre-enqueued messages/jobs
- end_to_end: enqueue + dequeue(+ack where relevant) in the same loop N times

Systems:
- rustqueue_http (REST)
- rustqueue_tcp (native TCP protocol)
- redis_list (LPUSH/RPOP)
- rabbitmq (basic_publish/basic_get/basic_ack)
- bullmq (Redis-backed Node queue)
- celery (Redis-backed Python task queue)
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Dict

import pika
import redis
import requests

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from celery_bench_app import app as celery_app
from celery_bench_app import noop as celery_noop


REPO_ROOT = SCRIPTS_DIR.parent
NODE_BENCH_DIR = SCRIPTS_DIR / "node-bench"
NODE_BENCH_SCRIPT = NODE_BENCH_DIR / "bullmq_bench.mjs"
RUSTQUEUE_BIN = REPO_ROOT / "target" / "release" / "rustqueue"
RUSTQUEUE_HTTP_PORT = 16790
RUSTQUEUE_TCP_PORT = 16789
REDIS_PORT = 6380
RABBIT_PORT = 5673
REDIS_CONTAINER = "rustqueue-bench-redis"
RABBIT_CONTAINER = "rustqueue-bench-rabbit"


@dataclass
class BenchResult:
    system: str
    produce_ops_s: float
    consume_ops_s: float
    end_to_end_ops_s: float


class RustQueueTCPClient:
    def __init__(self, host: str, port: int) -> None:
        self.host = host
        self.port = port
        self.sock: socket.socket | None = None
        self.file = None

    def __enter__(self) -> "RustQueueTCPClient":
        self.sock = socket.create_connection((self.host, self.port), timeout=5)
        self.file = self.sock.makefile("rwb")
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self.file is not None:
            self.file.close()
        if self.sock is not None:
            self.sock.close()

    def cmd(self, payload: dict) -> dict:
        assert self.file is not None
        line = json.dumps(payload, separators=(",", ":")).encode("utf-8") + b"\n"
        self.file.write(line)
        self.file.flush()
        resp = self.file.readline()
        if not resp:
            raise RuntimeError("tcp connection closed")
        body = json.loads(resp)
        if not body.get("ok", False):
            raise RuntimeError(f"tcp command failed: {body}")
        return body


def run(cmd: list[str], *, check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        check=check,
        text=True,
        capture_output=capture,
    )


def stop_container(name: str) -> None:
    run(["docker", "rm", "-f", name], check=False)


def ensure_node_bench_dependencies() -> None:
    required = NODE_BENCH_DIR / "node_modules" / "bullmq"
    if required.exists():
        return
    run(["npm", "install", "--prefix", str(NODE_BENCH_DIR)], capture=False)


def start_redis() -> None:
    stop_container(REDIS_CONTAINER)
    run(
        [
            "docker",
            "run",
            "-d",
            "--rm",
            "--name",
            REDIS_CONTAINER,
            "-p",
            f"{REDIS_PORT}:6379",
            "redis:7-alpine",
        ]
    )
    client = redis.Redis(host="127.0.0.1", port=REDIS_PORT, decode_responses=False)
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            if client.ping():
                return
        except Exception:
            pass
        time.sleep(0.2)
    raise RuntimeError("redis did not become ready")


def start_rabbitmq() -> None:
    stop_container(RABBIT_CONTAINER)
    run(
        [
            "docker",
            "run",
            "-d",
            "--rm",
            "--name",
            RABBIT_CONTAINER,
            "-p",
            f"{RABBIT_PORT}:5672",
            "rabbitmq:3.13-alpine",
        ]
    )
    deadline = time.time() + 60
    while time.time() < deadline:
        try:
            conn = pika.BlockingConnection(
                pika.ConnectionParameters(host="127.0.0.1", port=RABBIT_PORT)
            )
            conn.close()
            return
        except Exception:
            time.sleep(0.5)
    raise RuntimeError("rabbitmq did not become ready")


def wait_for_rustqueue() -> None:
    deadline = time.time() + 30
    url = f"http://127.0.0.1:{RUSTQUEUE_HTTP_PORT}/api/v1/health"
    while time.time() < deadline:
        try:
            r = requests.get(url, timeout=1)
            if r.status_code == 200:
                return
        except Exception:
            pass
        time.sleep(0.2)
    raise RuntimeError("rustqueue did not become ready")


def start_rustqueue_server(redb_durability: str, write_coalescing: bool = False, hybrid: bool = False) -> tuple[subprocess.Popen, Path]:
    if not RUSTQUEUE_BIN.exists():
        run(["cargo", "build", "--release"], capture=False)

    tmpdir = Path(tempfile.mkdtemp(prefix="rustqueue-bench-"))
    data_dir = tmpdir / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    cfg_path = tmpdir / "rustqueue.toml"

    if hybrid:
        storage_lines = [
            "[storage]",
            'backend = "hybrid"',
            f'path = "{data_dir.as_posix()}"',
            f'redb_durability = "{redb_durability}"',
            "hybrid_snapshot_interval_ms = 1000",
            "hybrid_max_dirty = 5000",
        ]
    else:
        storage_lines = [
            "[storage]",
            'backend = "redb"',
            f'path = "{data_dir.as_posix()}"',
            f'redb_durability = "{redb_durability}"',
        ]
    if write_coalescing:
        storage_lines.extend([
            "write_coalescing_enabled = true",
            "write_coalescing_interval_ms = 10",
            "write_coalescing_max_batch = 100",
        ])

    cfg_path.write_text(
        "\n".join(
            [
                "[server]",
                'host = "127.0.0.1"',
                f"http_port = {RUSTQUEUE_HTTP_PORT}",
                f"tcp_port = {RUSTQUEUE_TCP_PORT}",
                "",
                *storage_lines,
                "",
                "[auth]",
                "enabled = false",
                "tokens = []",
            ]
        ),
        encoding="utf-8",
    )

    proc = subprocess.Popen(
        [
            str(RUSTQUEUE_BIN),
            "serve",
            "--config",
            str(cfg_path),
            "--http-port",
            str(RUSTQUEUE_HTTP_PORT),
            "--tcp-port",
            str(RUSTQUEUE_TCP_PORT),
        ],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env={**os.environ, "RUST_LOG": "warn"},
    )

    try:
        wait_for_rustqueue()
    except Exception:
        out = ""
        if proc.stdout is not None:
            try:
                out = proc.stdout.read()
            except Exception:
                pass
        proc.kill()
        raise RuntimeError(f"failed to start rustqueue:\n{out}")

    return proc, tmpdir


def start_celery_worker() -> subprocess.Popen:
    env = {
        **os.environ,
        "PYTHONPATH": f"{SCRIPTS_DIR}{os.pathsep}{os.environ.get('PYTHONPATH', '')}",
        "RUSTQUEUE_BENCH_REDIS_PORT": str(REDIS_PORT),
    }
    proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "celery",
            "-A",
            "celery_bench_app",
            "worker",
            "--pool",
            "solo",
            "--concurrency",
            "1",
            "--loglevel",
            "WARNING",
            "--without-gossip",
            "--without-mingle",
            "--without-heartbeat",
        ],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=env,
    )

    deadline = time.time() + 45
    while time.time() < deadline:
        if proc.poll() is not None:
            break
        try:
            pong = celery_app.control.ping(timeout=1)
            if pong:
                return proc
        except Exception:
            pass
        time.sleep(0.5)

    out = ""
    if proc.stdout is not None:
        try:
            out = proc.stdout.read()
        except Exception:
            pass
    proc.kill()
    raise RuntimeError(f"celery worker did not become ready:\n{out}")


def stop_celery_worker(proc: subprocess.Popen | None) -> None:
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=8)
    except subprocess.TimeoutExpired:
        proc.kill()


def purge_celery() -> None:
    try:
        celery_app.control.purge()
    except Exception:
        pass


def ops_per_s(n: int, duration_s: float) -> float:
    return float(n) / duration_s if duration_s > 0 else 0.0


def bench_rustqueue_http(n: int, remove_on_complete: bool) -> BenchResult:
    base = f"http://127.0.0.1:{RUSTQUEUE_HTTP_PORT}"
    session = requests.Session()

    def push_payload(i: int) -> dict:
        payload = {"name": "bench", "data": {"i": i}}
        if remove_on_complete:
            payload["remove_on_complete"] = True
        return payload

    def produce(queue: str, count: int) -> float:
        t0 = time.perf_counter()
        for i in range(count):
            r = session.post(
                f"{base}/api/v1/queues/{queue}/jobs",
                json=push_payload(i),
                timeout=10,
            )
            r.raise_for_status()
        return time.perf_counter() - t0

    def consume(queue: str, count: int) -> float:
        t0 = time.perf_counter()
        for _ in range(count):
            r = session.get(
                f"{base}/api/v1/queues/{queue}/jobs",
                params={"count": 1},
                timeout=10,
            )
            r.raise_for_status()
            payload = r.json()
            job = payload.get("job")
            if not job:
                raise RuntimeError(f"http pull returned no job: {payload}")
            job_id = job["id"]
            ack = session.post(f"{base}/api/v1/jobs/{job_id}/ack", json={}, timeout=10)
            ack.raise_for_status()
        return time.perf_counter() - t0

    queue = f"bench-http-produce-{int(time.time() * 1000)}"
    produce_dur = produce(queue, n)

    queue = f"bench-http-consume-{int(time.time() * 1000)}"
    produce(queue, n)
    consume_dur = consume(queue, n)

    queue = f"bench-http-e2e-{int(time.time() * 1000)}"
    t0 = time.perf_counter()
    for i in range(n):
        r = session.post(
            f"{base}/api/v1/queues/{queue}/jobs",
            json=push_payload(i),
            timeout=10,
        )
        r.raise_for_status()
        pull = session.get(
            f"{base}/api/v1/queues/{queue}/jobs",
            params={"count": 1},
            timeout=10,
        )
        pull.raise_for_status()
        job = pull.json().get("job")
        if not job:
            raise RuntimeError("http roundtrip pull returned no job")
        ack = session.post(f"{base}/api/v1/jobs/{job['id']}/ack", json={}, timeout=10)
        ack.raise_for_status()
    e2e_dur = time.perf_counter() - t0

    return BenchResult(
        system="rustqueue_http",
        produce_ops_s=ops_per_s(n, produce_dur),
        consume_ops_s=ops_per_s(n, consume_dur),
        end_to_end_ops_s=ops_per_s(n, e2e_dur),
    )


def bench_rustqueue_tcp(n: int, remove_on_complete: bool, batch_size: int) -> BenchResult:
    if batch_size < 1:
        raise ValueError("batch_size must be >= 1")

    with RustQueueTCPClient("127.0.0.1", RUSTQUEUE_TCP_PORT) as client:
        def push_cmd(queue: str, i: int) -> dict:
            payload = {"cmd": "push", "queue": queue, "name": "bench", "data": {"i": i}}
            if remove_on_complete:
                payload["remove_on_complete"] = True
            return payload

        def push_batch_cmd(queue: str, start: int, count: int) -> dict:
            jobs = []
            for i in range(start, start + count):
                item = {"name": "bench", "data": {"i": i}}
                if remove_on_complete:
                    item["remove_on_complete"] = True
                jobs.append(item)
            return {"cmd": "push_batch", "queue": queue, "jobs": jobs}

        queue = f"bench-tcp-produce-{int(time.time() * 1000)}"
        t0 = time.perf_counter()
        if batch_size == 1:
            for i in range(n):
                client.cmd(push_cmd(queue, i))
        else:
            i = 0
            while i < n:
                count = min(batch_size, n - i)
                client.cmd(push_batch_cmd(queue, i, count))
                i += count
        produce_dur = time.perf_counter() - t0

        queue = f"bench-tcp-consume-{int(time.time() * 1000)}"
        if batch_size == 1:
            for i in range(n):
                client.cmd(push_cmd(queue, i))
        else:
            i = 0
            while i < n:
                count = min(batch_size, n - i)
                client.cmd(push_batch_cmd(queue, i, count))
                i += count

        t0 = time.perf_counter()
        if batch_size == 1:
            for _ in range(n):
                pulled = client.cmd({"cmd": "pull", "queue": queue})
                job = pulled.get("job")
                if not job:
                    raise RuntimeError(f"tcp pull returned no job: {pulled}")
                client.cmd({"cmd": "ack", "id": job["id"]})
        else:
            remaining = n
            while remaining > 0:
                count = min(batch_size, remaining)
                pulled = client.cmd({"cmd": "pull", "queue": queue, "count": count})
                jobs = pulled.get("jobs")
                if not jobs:
                    raise RuntimeError(f"tcp batch pull returned no jobs: {pulled}")
                ack_items = [{"id": job["id"]} for job in jobs]
                acked = client.cmd({"cmd": "ack_batch", "items": ack_items})
                if "results" not in acked:
                    raise RuntimeError(f"tcp ack_batch returned invalid response: {acked}")
                remaining -= len(jobs)
        consume_dur = time.perf_counter() - t0

        queue = f"bench-tcp-e2e-{int(time.time() * 1000)}"
        t0 = time.perf_counter()
        if batch_size == 1:
            for i in range(n):
                client.cmd(push_cmd(queue, i))
                pulled = client.cmd({"cmd": "pull", "queue": queue})
                job = pulled.get("job")
                if not job:
                    raise RuntimeError("tcp roundtrip pull returned no job")
                client.cmd({"cmd": "ack", "id": job["id"]})
        else:
            i = 0
            while i < n:
                count = min(batch_size, n - i)
                client.cmd(push_batch_cmd(queue, i, count))
                pulled = client.cmd({"cmd": "pull", "queue": queue, "count": count})
                jobs = pulled.get("jobs")
                if not jobs:
                    raise RuntimeError("tcp roundtrip batch pull returned no jobs")
                ack_items = [{"id": job["id"]} for job in jobs]
                acked = client.cmd({"cmd": "ack_batch", "items": ack_items})
                if "results" not in acked:
                    raise RuntimeError(f"tcp roundtrip ack_batch invalid response: {acked}")
                i += len(jobs)
        e2e_dur = time.perf_counter() - t0

    return BenchResult(
        system="rustqueue_tcp",
        produce_ops_s=ops_per_s(n, produce_dur),
        consume_ops_s=ops_per_s(n, consume_dur),
        end_to_end_ops_s=ops_per_s(n, e2e_dur),
    )


def bench_redis_list(n: int) -> BenchResult:
    client = redis.Redis(host="127.0.0.1", port=REDIS_PORT, decode_responses=False)

    key = f"bench:redis:produce:{int(time.time() * 1000)}"
    t0 = time.perf_counter()
    for i in range(n):
        client.lpush(key, f"{i}".encode("utf-8"))
    produce_dur = time.perf_counter() - t0

    key = f"bench:redis:consume:{int(time.time() * 1000)}"
    for i in range(n):
        client.lpush(key, f"{i}".encode("utf-8"))
    t0 = time.perf_counter()
    for _ in range(n):
        v = client.rpop(key)
        if v is None:
            raise RuntimeError("redis rpop returned no value")
    consume_dur = time.perf_counter() - t0

    key = f"bench:redis:e2e:{int(time.time() * 1000)}"
    t0 = time.perf_counter()
    for i in range(n):
        client.lpush(key, f"{i}".encode("utf-8"))
        v = client.rpop(key)
        if v is None:
            raise RuntimeError("redis roundtrip rpop returned no value")
    e2e_dur = time.perf_counter() - t0

    return BenchResult(
        system="redis_list",
        produce_ops_s=ops_per_s(n, produce_dur),
        consume_ops_s=ops_per_s(n, consume_dur),
        end_to_end_ops_s=ops_per_s(n, e2e_dur),
    )


def bench_rabbitmq(n: int) -> BenchResult:
    conn = pika.BlockingConnection(
        pika.ConnectionParameters(host="127.0.0.1", port=RABBIT_PORT)
    )
    ch = conn.channel()

    queue = f"bench.rabbit.produce.{int(time.time() * 1000)}"
    ch.queue_declare(queue=queue, durable=False, auto_delete=True)
    t0 = time.perf_counter()
    for i in range(n):
        ch.basic_publish(exchange="", routing_key=queue, body=str(i).encode("utf-8"))
    produce_dur = time.perf_counter() - t0

    queue = f"bench.rabbit.consume.{int(time.time() * 1000)}"
    ch.queue_declare(queue=queue, durable=False, auto_delete=True)
    for i in range(n):
        ch.basic_publish(exchange="", routing_key=queue, body=str(i).encode("utf-8"))
    t0 = time.perf_counter()
    for _ in range(n):
        method, _, body = ch.basic_get(queue=queue, auto_ack=False)
        if method is None or body is None:
            raise RuntimeError("rabbitmq basic_get returned no message")
        ch.basic_ack(method.delivery_tag)
    consume_dur = time.perf_counter() - t0

    queue = f"bench.rabbit.e2e.{int(time.time() * 1000)}"
    ch.queue_declare(queue=queue, durable=False, auto_delete=True)
    t0 = time.perf_counter()
    for i in range(n):
        ch.basic_publish(exchange="", routing_key=queue, body=str(i).encode("utf-8"))
        method, _, body = ch.basic_get(queue=queue, auto_ack=False)
        if method is None or body is None:
            raise RuntimeError("rabbitmq roundtrip basic_get returned no message")
        ch.basic_ack(method.delivery_tag)
    e2e_dur = time.perf_counter() - t0

    conn.close()

    return BenchResult(
        system="rabbitmq",
        produce_ops_s=ops_per_s(n, produce_dur),
        consume_ops_s=ops_per_s(n, consume_dur),
        end_to_end_ops_s=ops_per_s(n, e2e_dur),
    )


def bench_bullmq(n: int) -> BenchResult:
    ensure_node_bench_dependencies()
    proc = run(
        [
            "node",
            str(NODE_BENCH_SCRIPT),
            "--ops",
            str(n),
            "--redis-port",
            str(REDIS_PORT),
        ]
    )
    lines = [ln.strip() for ln in proc.stdout.splitlines() if ln.strip()]
    if not lines:
        raise RuntimeError("bullmq benchmark returned no output")
    payload = json.loads(lines[-1])
    return BenchResult(
        system="bullmq",
        produce_ops_s=float(payload["produce_ops_s"]),
        consume_ops_s=float(payload["consume_ops_s"]),
        end_to_end_ops_s=float(payload["end_to_end_ops_s"]),
    )


def bench_celery(n: int) -> BenchResult:
    purge_celery()

    # Produce throughput: enqueue only (no worker required).
    t0 = time.perf_counter()
    for i in range(n):
        celery_noop.delay(i)
    produce_dur = time.perf_counter() - t0
    purge_celery()

    worker = start_celery_worker()
    try:
        purge_celery()

        # Consume throughput: prefill, then drain with ack/result collection.
        async_results = []
        for i in range(n):
            async_results.append(celery_noop.delay(i))
        t0 = time.perf_counter()
        for result in async_results:
            result.get(timeout=120)
        consume_dur = time.perf_counter() - t0

        purge_celery()

        # End-to-end: enqueue then wait completion for each item.
        t0 = time.perf_counter()
        for i in range(n):
            celery_noop.delay(i).get(timeout=120)
        e2e_dur = time.perf_counter() - t0

        purge_celery()
    finally:
        stop_celery_worker(worker)

    return BenchResult(
        system="celery",
        produce_ops_s=ops_per_s(n, produce_dur),
        consume_ops_s=ops_per_s(n, consume_dur),
        end_to_end_ops_s=ops_per_s(n, e2e_dur),
    )


def leaderboard(results: list[BenchResult], field: str) -> list[str]:
    ordered = sorted(results, key=lambda r: getattr(r, field), reverse=True)
    return [r.system for r in ordered]


def aggregate_results(runs: list[list[BenchResult]]) -> list[BenchResult]:
    by_system: dict[str, dict[str, list[float]]] = {}
    for run_results in runs:
        for result in run_results:
            entry = by_system.setdefault(
                result.system,
                {"produce": [], "consume": [], "e2e": []},
            )
            entry["produce"].append(result.produce_ops_s)
            entry["consume"].append(result.consume_ops_s)
            entry["e2e"].append(result.end_to_end_ops_s)

    aggregated: list[BenchResult] = []
    for system, metrics in by_system.items():
        aggregated.append(
            BenchResult(
                system=system,
                produce_ops_s=float(statistics.median(metrics["produce"])),
                consume_ops_s=float(statistics.median(metrics["consume"])),
                end_to_end_ops_s=float(statistics.median(metrics["e2e"])),
            )
        )
    return aggregated


def write_report(
    results: list[BenchResult],
    raw_runs: list[list[BenchResult]],
    n: int,
    redb_durability: str,
    remove_on_complete: bool,
    rustqueue_tcp_batch_size: int,
    repeats: int,
    write_coalescing: bool = False,
    hybrid: bool = False,
) -> tuple[Path, Path]:
    today = dt.date.today().isoformat()
    md_path = REPO_ROOT / "docs" / f"competitor-benchmark-{today}.md"
    json_path = REPO_ROOT / "docs" / f"competitor-benchmark-{today}.json"

    payload: Dict[str, object] = {
        "date": today,
        "operations_per_test": n,
        "repeats": repeats,
        "rustqueue_redb_durability": redb_durability,
        "rustqueue_remove_on_complete": remove_on_complete,
        "rustqueue_tcp_batch_size": rustqueue_tcp_batch_size,
        "rustqueue_write_coalescing": write_coalescing,
        "rustqueue_hybrid": hybrid,
        "raw_runs": [[r.__dict__ for r in run] for run in raw_runs],
        "results": [r.__dict__ for r in results],
    }
    json_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    winners = {
        "produce": leaderboard(results, "produce_ops_s")[0],
        "consume": leaderboard(results, "consume_ops_s")[0],
        "end_to_end": leaderboard(results, "end_to_end_ops_s")[0],
    }

    lines = [
        f"# RustQueue Competitor Benchmark ({today})",
        "",
        "## Method",
        "",
        "Sequential local benchmark on one machine.",
        f"Each metric runs **{n} operations** per system.",
        f"Runs per system/metric: **{repeats}** (reported value = median).",
        f"RustQueue redb durability mode: **{redb_durability}**.",
        f"RustQueue remove_on_complete: **{remove_on_complete}**.",
        f"RustQueue TCP batch size: **{rustqueue_tcp_batch_size}**.",
        f"RustQueue write coalescing: **{write_coalescing}**.",
        f"RustQueue hybrid storage: **{hybrid}**.",
        "",
        "Metrics:",
        "- `produce_ops_s`: enqueue throughput",
        "- `consume_ops_s`: dequeue(+ack where relevant) throughput from a prefilled queue",
        "- `end_to_end_ops_s`: enqueue + dequeue(+ack where relevant) in the same loop",
        "",
        "Systems:",
        "- `rustqueue_http`: RustQueue REST API",
        "- `rustqueue_tcp`: RustQueue TCP protocol",
        "- `redis_list`: Redis LPUSH/RPOP",
        "- `rabbitmq`: RabbitMQ basic_publish/basic_get/basic_ack",
        "- `bullmq`: BullMQ on Redis",
        "- `celery`: Celery on Redis",
        "",
        "## Results",
        "",
        "| System | Produce ops/s | Consume ops/s | End-to-end ops/s |",
        "|--------|---------------:|--------------:|-----------------:|",
    ]

    for r in sorted(results, key=lambda x: x.end_to_end_ops_s, reverse=True):
        lines.append(
            f"| {r.system} | {r.produce_ops_s:,.0f} | {r.consume_ops_s:,.0f} | {r.end_to_end_ops_s:,.0f} |"
        )

    lines.extend(
        [
            "",
            "## Winners",
            "",
            f"- Produce winner: `{winners['produce']}`",
            f"- Consume winner: `{winners['consume']}`",
            f"- End-to-end winner: `{winners['end_to_end']}`",
            "",
            "## Notes",
            "",
        "- This compares local defaults and protocol-level behavior, not managed cloud offerings.",
        f"- RustQueue storage is `redb` with durability mode `{redb_durability}`; competitor durability semantics vary.",
        f"- RustQueue jobs are enqueued with `remove_on_complete={str(remove_on_complete).lower()}` in this run.",
        f"- RustQueue TCP benchmark used `batch_size={rustqueue_tcp_batch_size}` (`push_batch`/`ack_batch` when >1).",
        f"- RustQueue write coalescing was **{'enabled' if write_coalescing else 'disabled'}** (buffers single push/ack into batched flushes when enabled).",
        f"- RustQueue hybrid storage was **{'enabled' if hybrid else 'disabled'}** (in-memory DashMap + periodic redb snapshots when enabled).",
        "- Celery and BullMQ include worker/runtime overhead in consume and end-to-end measurements.",
        "- For stricter apples-to-apples durability, run an additional profile with explicit fsync/persistence settings on each system.",
        ]
    )

    md_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return md_path, json_path


def main() -> int:
    parser = argparse.ArgumentParser(description="Benchmark RustQueue vs local competitors")
    parser.add_argument("--ops", type=int, default=1000, help="operations per metric")
    parser.add_argument(
        "--redb-durability",
        choices=["none", "immediate", "eventual"],
        default="immediate",
        help="RustQueue redb durability mode",
    )
    parser.add_argument(
        "--remove-on-complete",
        action="store_true",
        help="Set remove_on_complete=true for RustQueue pushed jobs",
    )
    parser.add_argument(
        "--repeats",
        type=int,
        default=1,
        help="number of full benchmark runs to aggregate (median)",
    )
    parser.add_argument(
        "--rustqueue-tcp-batch-size",
        type=int,
        default=1,
        help="batch size for RustQueue TCP benchmark (uses push_batch/ack_batch when >1)",
    )
    parser.add_argument(
        "--write-coalescing",
        action="store_true",
        help="Enable RustQueue write coalescing (buffers single push/ack into batched flushes)",
    )
    parser.add_argument(
        "--hybrid",
        action="store_true",
        help="Use hybrid memory+disk storage (in-memory DashMap + periodic redb snapshots)",
    )
    args = parser.parse_args()

    n = args.ops
    redb_durability = args.redb_durability
    remove_on_complete = args.remove_on_complete
    repeats = args.repeats
    rustqueue_tcp_batch_size = args.rustqueue_tcp_batch_size
    write_coalescing = args.write_coalescing
    hybrid = args.hybrid
    rustqueue_proc = None
    tmpdir = None

    try:
        start_redis()
        start_rabbitmq()
        rustqueue_proc, tmpdir = start_rustqueue_server(redb_durability, write_coalescing, hybrid)

        run_results: list[list[BenchResult]] = []
        for _ in range(repeats):
            run_results.append(
                [
                    bench_rustqueue_http(n, remove_on_complete),
                    bench_rustqueue_tcp(n, remove_on_complete, rustqueue_tcp_batch_size),
                    bench_redis_list(n),
                    bench_rabbitmq(n),
                    bench_bullmq(n),
                    bench_celery(n),
                ]
            )

        results = aggregate_results(run_results)

        md_path, json_path = write_report(
            results,
            run_results,
            n,
            redb_durability,
            remove_on_complete,
            rustqueue_tcp_batch_size,
            repeats,
            write_coalescing,
            hybrid,
        )

        print("Benchmark complete")
        print(f"Markdown report: {md_path}")
        print(f"JSON report: {json_path}")
        for r in results:
            print(
                f"{r.system}: produce={r.produce_ops_s:.0f}/s, consume={r.consume_ops_s:.0f}/s, e2e={r.end_to_end_ops_s:.0f}/s"
            )
        return 0
    finally:
        if rustqueue_proc is not None:
            rustqueue_proc.terminate()
            try:
                rustqueue_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                rustqueue_proc.kill()
        stop_container(REDIS_CONTAINER)
        stop_container(RABBIT_CONTAINER)
        if tmpdir is not None:
            try:
                for p in sorted(tmpdir.rglob("*"), reverse=True):
                    if p.is_file() or p.is_symlink():
                        p.unlink(missing_ok=True)
                    elif p.is_dir():
                        p.rmdir()
                tmpdir.rmdir()
            except Exception:
                pass


if __name__ == "__main__":
    sys.exit(main())
