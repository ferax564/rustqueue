"""Celery app used by benchmark_competitors.py."""

from __future__ import annotations

import os

from celery import Celery

redis_port = os.environ.get("RUSTQUEUE_BENCH_REDIS_PORT", "6380")
redis_url = f"redis://127.0.0.1:{redis_port}/0"

app = Celery("celery_bench", broker=redis_url, backend=redis_url)
app.conf.update(
    task_serializer="json",
    accept_content=["json"],
    result_serializer="json",
    task_acks_late=False,
    task_ignore_result=False,
)


@app.task(name="celery_bench.noop")
def noop(value: int) -> int:
    return value
