"""HTTP client for RustQueue — background jobs without infrastructure.

Uses only Python standard library (``urllib.request``, ``json``,
``urllib.parse``) so there are zero external dependencies.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Optional, Union


from rustqueue.errors import RustQueueError


class RustQueueClient:
    """Synchronous HTTP client for a RustQueue server.

    Args:
        base_url: Root URL of the RustQueue HTTP API (default ``http://localhost:6790``).
        token: Optional bearer token used for authentication.
        timeout: Request timeout in seconds (default 30).

    Example::

        client = RustQueueClient("http://localhost:6790", token="secret")
        job_id = client.push("emails", "send-welcome", {"user_id": 42})
    """

    def __init__(
        self,
        base_url: str = "http://localhost:6790",
        token: Optional[str] = None,
        timeout: int = 30,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._token = token
        self._timeout = timeout

    # ── internal helpers ─────────────────────────────────────────────────

    def _build_url(self, path: str, query: Optional[Dict[str, str]] = None) -> str:
        """Build a full URL from *path* and optional query parameters."""
        url = self._base_url + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        return url

    def _request(
        self,
        method: str,
        path: str,
        body: Any = None,
        query: Optional[Dict[str, str]] = None,
    ) -> Any:
        """Send an HTTP request and return the parsed JSON response.

        Raises:
            RustQueueError: If the server returns ``{"ok": false, ...}``.
            urllib.error.URLError: On network-level failures.
        """
        url = self._build_url(path, query)
        data: Optional[bytes] = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")

        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "application/json")
        if self._token is not None:
            req.add_header("Authorization", f"Bearer {self._token}")

        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                resp_body = resp.read().decode("utf-8")
                if not resp_body:
                    return {"ok": True}
                return json.loads(resp_body)
        except urllib.error.HTTPError as exc:
            # Try to parse the error body for a structured RustQueue error.
            try:
                err_body = json.loads(exc.read().decode("utf-8"))
            except Exception:
                raise RustQueueError("HTTP_ERROR", f"HTTP {exc.code}: {exc.reason}") from exc
            if isinstance(err_body, dict) and "error" in err_body:
                err = err_body["error"]
                raise RustQueueError(
                    err.get("code", "UNKNOWN"),
                    err.get("message", str(err)),
                ) from exc
            raise RustQueueError("HTTP_ERROR", f"HTTP {exc.code}: {exc.reason}") from exc

    # ── job operations ───────────────────────────────────────────────────

    def push(
        self,
        queue: str,
        name: str,
        data: Optional[Dict[str, Any]] = None,
        *,
        priority: Optional[int] = None,
        delay_ms: Optional[int] = None,
        max_attempts: Optional[int] = None,
        backoff: Optional[str] = None,
        backoff_delay_ms: Optional[int] = None,
        ttl_ms: Optional[int] = None,
        timeout_ms: Optional[int] = None,
        unique_key: Optional[str] = None,
        tags: Optional[List[str]] = None,
        group_id: Optional[str] = None,
        lifo: Optional[bool] = None,
        remove_on_complete: Optional[bool] = None,
        remove_on_fail: Optional[bool] = None,
        custom_id: Optional[str] = None,
    ) -> str:
        """Push a single job onto *queue* and return its ID.

        Args:
            queue: Target queue name.
            name: Job type name.
            data: Arbitrary JSON-serialisable payload (default empty object).
            priority: Numeric priority (lower = higher priority).
            delay_ms: Delay before the job becomes eligible, in milliseconds.
            max_attempts: Maximum number of delivery attempts.
            backoff: Backoff strategy (``"fixed"``, ``"exponential"``).
            backoff_delay_ms: Base delay between retries, in milliseconds.
            ttl_ms: Time-to-live for the job, in milliseconds.
            timeout_ms: Per-attempt execution timeout, in milliseconds.
            unique_key: Deduplication key; only one active job per key.
            tags: Arbitrary string tags for filtering.
            group_id: Group identifier for ordered processing.
            lifo: If ``True``, the job is processed last-in-first-out.
            remove_on_complete: Automatically delete the job after completion.
            remove_on_fail: Automatically delete the job after final failure.
            custom_id: Caller-provided job ID (must be a valid UUID).

        Returns:
            The job ID as a string.
        """
        payload: Dict[str, Any] = {"name": name, "data": data or {}}
        _set_optional(payload, "priority", priority)
        _set_optional(payload, "delay_ms", delay_ms)
        _set_optional(payload, "max_attempts", max_attempts)
        _set_optional(payload, "backoff", backoff)
        _set_optional(payload, "backoff_delay_ms", backoff_delay_ms)
        _set_optional(payload, "ttl_ms", ttl_ms)
        _set_optional(payload, "timeout_ms", timeout_ms)
        _set_optional(payload, "unique_key", unique_key)
        _set_optional(payload, "tags", tags)
        _set_optional(payload, "group_id", group_id)
        _set_optional(payload, "lifo", lifo)
        _set_optional(payload, "remove_on_complete", remove_on_complete)
        _set_optional(payload, "remove_on_fail", remove_on_fail)
        _set_optional(payload, "custom_id", custom_id)

        resp = self._request("POST", f"/api/v1/queues/{_enc(queue)}/jobs", body=payload)
        return resp["id"]

    def push_batch(self, queue: str, jobs: List[Dict[str, Any]]) -> List[str]:
        """Push multiple jobs onto *queue* in a single request.

        Each item in *jobs* must be a dict with at least ``name`` and ``data``
        keys.  Additional keys are passed through as job options (see
        :meth:`push` for the full list).

        Returns:
            A list of job IDs in the same order as the input.
        """
        resp = self._request("POST", f"/api/v1/queues/{_enc(queue)}/jobs", body=jobs)
        return resp["ids"]

    def pull(self, queue: str, count: int = 1) -> List[Dict[str, Any]]:
        """Pull up to *count* ready jobs from *queue*.

        When *count* is 1 (the default), the server returns a single-job
        envelope.  This method normalises both shapes into a plain list so
        the caller always receives ``list[dict]``.

        Returns:
            A (possibly empty) list of job dicts.
        """
        query: Optional[Dict[str, str]] = None
        if count != 1:
            query = {"count": str(count)}
        resp = self._request("GET", f"/api/v1/queues/{_enc(queue)}/jobs", query=query)

        # Single-pull: {"ok": true, "job": {...} | null}
        if "job" in resp:
            job = resp["job"]
            return [job] if job is not None else []

        # Multi-pull: {"ok": true, "jobs": [...]}
        return resp.get("jobs", [])

    def ack(self, job_id: str, result: Optional[Any] = None) -> None:
        """Acknowledge successful completion of a job.

        Args:
            job_id: The job's ID.
            result: Optional result payload to store with the completed job.
        """
        body: Dict[str, Any] = {}
        if result is not None:
            body["result"] = result
        self._request("POST", f"/api/v1/jobs/{_enc(job_id)}/ack", body=body)

    def fail(self, job_id: str, error: str) -> Dict[str, Any]:
        """Report that a job has failed.

        Args:
            job_id: The job's ID.
            error: Human-readable error description.

        Returns:
            A dict with ``retry`` (bool) and optionally ``next_attempt_at``.
        """
        resp = self._request("POST", f"/api/v1/jobs/{_enc(job_id)}/fail", body={"error": error})
        return {"retry": resp.get("retry", False), "next_attempt_at": resp.get("next_attempt_at")}

    def cancel(self, job_id: str) -> None:
        """Cancel a pending or delayed job.

        Args:
            job_id: The job's ID.
        """
        self._request("POST", f"/api/v1/jobs/{_enc(job_id)}/cancel", body={})

    def progress(self, job_id: str, progress: int, message: Optional[str] = None) -> None:
        """Update the progress of an active job.

        Args:
            job_id: The job's ID.
            progress: Integer between 0 and 100.
            message: Optional human-readable status message.
        """
        body: Dict[str, Any] = {"progress": progress}
        if message is not None:
            body["message"] = message
        self._request("POST", f"/api/v1/jobs/{_enc(job_id)}/progress", body=body)

    def heartbeat(self, job_id: str) -> None:
        """Send a heartbeat for an active job to prevent stall detection.

        Args:
            job_id: The job's ID.
        """
        self._request("POST", f"/api/v1/jobs/{_enc(job_id)}/heartbeat", body={})

    def get_job(self, job_id: str) -> Optional[Dict[str, Any]]:
        """Fetch a job by ID.

        Returns:
            The job dict, or ``None`` if the job does not exist.
        """
        try:
            resp = self._request("GET", f"/api/v1/jobs/{_enc(job_id)}")
            return resp.get("job")
        except RustQueueError as exc:
            if exc.code == "JOB_NOT_FOUND":
                return None
            raise

    # ── queue operations ─────────────────────────────────────────────────

    def list_queues(self) -> List[Dict[str, Any]]:
        """List all known queues with their counts.

        Returns:
            A list of queue info dicts, each containing ``name`` and ``counts``.
        """
        resp = self._request("GET", "/api/v1/queues")
        return resp.get("queues", [])

    def get_queue_stats(self, queue: str) -> Dict[str, Any]:
        """Get statistics for a single queue.

        Returns:
            A dict of state counts (``waiting``, ``active``, ``completed``, etc.).
        """
        resp = self._request("GET", f"/api/v1/queues/{_enc(queue)}/stats")
        return resp.get("counts", {})

    def get_dlq_jobs(self, queue: str, limit: int = 100) -> List[Dict[str, Any]]:
        """List jobs in the dead-letter queue for *queue*.

        Args:
            limit: Maximum number of jobs to return (default 100).

        Returns:
            A list of job dicts.
        """
        query = {"limit": str(limit)}
        resp = self._request("GET", f"/api/v1/queues/{_enc(queue)}/dlq", query=query)
        return resp.get("jobs", [])

    # ── schedule operations ──────────────────────────────────────────────

    def create_schedule(self, schedule: Dict[str, Any]) -> Dict[str, Any]:
        """Create (or upsert) a schedule.

        *schedule* must contain at least ``name``, ``queue``, and
        ``job_name``.  Optional keys: ``job_data``, ``job_options``,
        ``cron_expr``, ``every_ms``, ``timezone``, ``max_executions``.

        Returns:
            The created schedule dict.
        """
        resp = self._request("POST", "/api/v1/schedules", body=schedule)
        return resp.get("schedule", {})

    def list_schedules(self) -> List[Dict[str, Any]]:
        """List all schedules.

        Returns:
            A list of schedule dicts.
        """
        resp = self._request("GET", "/api/v1/schedules")
        return resp.get("schedules", [])

    def get_schedule(self, name: str) -> Optional[Dict[str, Any]]:
        """Fetch a schedule by name.

        Returns:
            The schedule dict, or ``None`` if not found.
        """
        try:
            resp = self._request("GET", f"/api/v1/schedules/{_enc(name)}")
            return resp.get("schedule")
        except RustQueueError as exc:
            if exc.code == "SCHEDULE_NOT_FOUND":
                return None
            raise

    def delete_schedule(self, name: str) -> None:
        """Delete a schedule by name."""
        self._request("DELETE", f"/api/v1/schedules/{_enc(name)}")

    def pause_schedule(self, name: str) -> None:
        """Pause a schedule so it stops creating new jobs."""
        self._request("POST", f"/api/v1/schedules/{_enc(name)}/pause", body={})

    def resume_schedule(self, name: str) -> None:
        """Resume a previously paused schedule."""
        self._request("POST", f"/api/v1/schedules/{_enc(name)}/resume", body={})

    # ── health ───────────────────────────────────────────────────────────

    def health(self) -> Dict[str, Any]:
        """Check server health.

        Returns:
            A dict with ``status``, ``version``, and ``uptime_seconds``.
        """
        return self._request("GET", "/api/v1/health")


# ── module-level helpers ─────────────────────────────────────────────────────


def _enc(value: str) -> str:
    """URL-encode a path segment."""
    return urllib.parse.quote(value, safe="")


def _set_optional(d: Dict[str, Any], key: str, value: Any) -> None:
    """Set *key* in *d* only if *value* is not ``None``."""
    if value is not None:
        d[key] = value
