# Product Requirements Document (PRD)
# **RustQueue** — A High-Performance Distributed Job Scheduler

**Version:** 1.0
**Date:** February 5, 2026
**Updated:** February 7, 2026
**Status:** Phase 1-8 (v0.13) Complete
**Author:** [Your Name]

---

## Table of Contents

1. [Vision & Opportunity](#1-vision--opportunity)
2. [Problem Statement](#2-problem-statement)
3. [Target Users & Personas](#3-target-users--personas)
4. [Product Overview](#4-product-overview)
5. [Competitive Analysis](#5-competitive-analysis)
6. [Core Principles & Design Philosophy](#6-core-principles--design-philosophy)
7. [Architecture](#7-architecture)
8. [Functional Requirements](#8-functional-requirements)
9. [Non-Functional Requirements](#9-non-functional-requirements)
10. [Data Model](#10-data-model)
11. [API Design](#11-api-design)
12. [SDK & Client Libraries](#12-sdk--client-libraries)
13. [Deployment & Configuration](#13-deployment--configuration)
14. [Security](#14-security)
15. [Observability & Monitoring](#15-observability--monitoring)
16. [Web Dashboard](#16-web-dashboard)
17. [Release Plan & Milestones](#17-release-plan--milestones)
18. [Success Metrics](#18-success-metrics)
19. [Business Model](#19-business-model)
20. [Risks & Mitigations](#20-risks--mitigations)
21. [Future Roadmap](#21-future-roadmap)
22. [Technical Stack](#22-technical-stack)
23. [Glossary](#23-glossary)

---

## Feature Status Legend

- ✅ **Implemented** — Working, tested, documented
- 🚧 **Partial** — Foundation exists, core logic incomplete
- 📋 **Roadmap** — Planned for a specific phase, not started

---

## 1. Vision & Opportunity

### 1.1 Vision Statement

Build the **SQLite of job scheduling** — a zero-dependency, single-binary, high-performance distributed job queue and task scheduler written in Rust that runs anywhere from a Raspberry Pi to a 100-node production cluster.

### 1.2 One-Liner

> The job scheduler that runs anywhere, scales to anything, and depends on nothing.

### 1.3 Market Opportunity

The global workload scheduling and automation market was valued at **$4.7 billion in 2025** and is projected to reach **$8.4 billion by 2035**, growing at a CAGR of 6.2% (OMR Global, 2025). Over 72% of enterprises operate more than 500 recurring batch jobs daily, while 48% manage over 5,000 workflows across hybrid infrastructure. Yet the current landscape forces teams to choose between:

- **Simplicity with limitations** (cron, in-process schedulers)
- **Power with complexity** (Temporal, Airflow, Kubernetes CronJobs)
- **Language lock-in** (Sidekiq for Ruby, Celery for Python, BullMQ for Node.js)
- **Infrastructure overhead** (Redis, RabbitMQ, PostgreSQL as mandatory dependencies)

There is no solution today that is simultaneously simple to deploy, language-agnostic, distributed, observable, and high-performance. RustQueue fills this gap.

### 1.4 Why Now

- The **White House and NSA** have formally recommended migration to memory-safe languages, creating institutional tailwinds for Rust-based infrastructure.
- **Rust adoption** has grown 40% year-over-year on GitHub, with 45% of organizations now using it for production workloads. Rust has been voted the "Most Admired Language" on Stack Overflow for 10 consecutive years.
- 80% of organizations are transitioning from traditional scheduling to cloud-driven automation and AI-powered orchestration (EMA Research, 2025).
- The "single binary" deployment model (popularized by Go tools like Caddy, Consul, and Litestream) has proven its market appeal, but Rust offers even better performance characteristics.
- **Serverless and edge computing** demand lightweight, embeddable tools — not heavy Java/Python runtimes.

### 1.5 Why Rust

| Advantage | Impact on Job Schedulers |
|---|---|
| No garbage collector | Zero GC pauses = precise job timing, predictable latency under load |
| Memory safety | Eliminates the #1 class of security bugs in long-running daemons |
| Single static binary | Zero runtime dependencies = trivial deployment |
| Async with tokio | Handles 100K+ concurrent connections efficiently |
| Low memory footprint | Runs on $5/month VPS or Raspberry Pi |
| WASM compilation | SDK can run in browsers for local-first apps |
| Embeddable as a library | Use as a server OR embed in your own Rust application |

---

## 2. Problem Statement

### 2.1 Problems with Unix Cron

Unix cron is the default for scheduled tasks and has remained virtually unchanged since the 1970s. Its limitations are well-documented:

- **No persistence.** If the server reboots during a scheduled window, the job is silently lost.
- **No retry logic.** A failed job simply fails. No backoff, no retry, no notification.
- **No observability.** Determining whether a job ran, when it ran, how long it took, or why it failed requires custom logging and ad-hoc tooling.
- **No job dependencies.** Cannot express "run B after A completes successfully."
- **No distributed coordination.** Running the same crontab on multiple servers causes duplicate execution.
- **No resource controls.** No concurrency limits, rate limiting, or priority queuing.
- **No dead letter queue.** Permanently failed jobs vanish without a trace.

### 2.2 Problems with Existing Solutions

| Solution | Primary Pain Points |
|---|---|
| **Celery** (Python) | Requires Redis or RabbitMQ. Python-only. Complex configuration. Memory-hungry. |
| **Sidekiq** (Ruby) | Ruby-only. Requires Redis. Pro features are expensive ($100+/month). |
| **BullMQ** (Node.js) | Requires Redis. JavaScript-only. GC pauses under load. |
| **Temporal** | Extremely powerful but massively complex. Requires a full cluster to operate. Steep learning curve. Overkill for 90% of use cases. |
| **Kubernetes CronJobs** | Requires Kubernetes. Pod startup overhead (seconds). Not suitable for sub-minute scheduling. |
| **Airflow** | Designed for data pipelines, not general job scheduling. Python-only. Heavy infrastructure. |
| **Cronicle** | Node.js-based. Single-threaded performance ceiling. Limited ecosystem. |
| **Dkron** | Go-based, distributed. But limited feature set — no job dependencies, no priorities, no DLQ. |
| **bunqueue** | Requires Bun runtime. Single-node only. TypeScript-only SDK. Not battle-tested. |
| **rust-task-queue** | Rust-based but requires Redis. Library only (no standalone server). No dashboard, no distributed mode, no DLQ. |

### 2.3 The Gap

No existing solution provides all of the following:

1. Zero external dependencies (no Redis, no PostgreSQL, no message broker)
2. Single-binary deployment
3. Language-agnostic interface (HTTP/TCP protocol, not tied to a runtime)
4. Distributed high availability without manual configuration
5. Production-grade observability out of the box
6. Embeddable as a library in other applications
7. Sub-millisecond scheduling precision
8. Built-in web dashboard

**RustQueue delivers all eight.**

---

## 3. Target Users & Personas

### 3.1 Primary Persona: The Backend Developer

**Name:** Alex — Senior Backend Engineer  
**Company size:** 10-200 engineers  
**Stack:** Polyglot (Go, Python, TypeScript, Rust)  
**Current solution:** Redis + BullMQ or Celery, with growing frustration over infrastructure complexity  

**Needs:**
- Drop-in background job processing for web applications
- Cron-like scheduling without managing crontab files across servers
- Retries with exponential backoff that just work
- A dashboard to see what's running, what failed, and why

**Quote:** *"I just want to schedule jobs. I don't want to manage Redis, monitor Redis, back up Redis, and deal with Redis memory issues just to run a background task every hour."*

### 3.2 Secondary Persona: The DevOps / Platform Engineer

**Name:** Jordan — Platform Engineer  
**Company size:** 50-500 engineers  
**Responsibility:** Internal infrastructure, developer tooling  
**Current solution:** Kubernetes CronJobs + custom monitoring  

**Needs:**
- Centralized job management across multiple services
- High availability without Kubernetes
- Prometheus/Grafana integration
- Authentication and multi-tenancy for different teams

**Quote:** *"We have 200 cron jobs scattered across 30 servers. Nobody knows which ones are still needed. When one fails, we find out from customers."*

### 3.3 Tertiary Persona: The Solo Developer / Startup

**Name:** Sam — Full-Stack Developer, Solo Founder  
**Company size:** 1-5  
**Stack:** Whatever works fastest  
**Current solution:** `setTimeout` in Node.js or cron on a $10 VPS  

**Needs:**
- Near-zero operational overhead
- Works on a cheap VPS
- Easy to set up, easy to forget about until something breaks
- Free for small scale

**Quote:** *"I need to send reminder emails, process uploads, and clean up expired sessions. I don't want to learn Kubernetes to do it."*

### 3.4 Stretch Persona: The Embedded / IoT Developer

**Name:** Casey — Embedded Systems Engineer  
**Stack:** Rust, C  
**Environment:** Resource-constrained devices, edge computing  

**Needs:**
- Embeddable library (not a separate server)
- Tiny memory footprint
- No network dependencies
- Deterministic timing

---

## 4. Product Overview

### 4.1 What RustQueue Is

RustQueue is a **job queue server and task scheduler** distributed as a single binary with zero external dependencies. It provides:

- **Job queuing** with priorities, delays, and FIFO/LIFO ordering
- **Cron scheduling** for recurring tasks
- **Automatic retries** with configurable backoff strategies
- **Dead letter queues** for inspecting and retrying permanently failed jobs
- **Job dependencies** (DAG-based workflows)
- **Real-time events** via WebSocket and Server-Sent Events
- **A built-in web dashboard** for monitoring and management
- **Prometheus metrics** for integration with existing monitoring stacks
- **Distributed mode** (optional) for high availability via Raft consensus
- **Embedded mode** for use as a Rust library without a separate server

### 4.2 What RustQueue Is Not

- **Not a message broker.** It is not a replacement for Kafka or RabbitMQ for event streaming. It is optimized for task execution, not pub/sub messaging.
- **Not a workflow orchestration platform.** It supports job dependencies and simple DAGs, but it is not a replacement for Temporal or Airflow for complex, long-running, multi-step workflows with compensation logic.
- **Not a data pipeline tool.** It does not provide ETL capabilities, data transformations, or dataset management.

### 4.3 Deployment Modes

| Mode | Description | Use Case |
|---|---|---|
| **Standalone** | Single binary, single node, embedded storage | Development, small apps, solo founders |
| **Server** | Single binary, single node, optional external storage | Production single-server deployments |
| **Cluster** | 3+ nodes with Raft consensus | High availability production |
| **Embedded** | Rust library (`use rustqueue::Queue`) | Integrating directly into Rust applications |

---

## 5. Competitive Analysis

### 5.1 Feature Comparison Matrix

| Feature | RustQueue | bunqueue | BullMQ | Sidekiq Pro | Celery | Temporal | Dkron | rust-task-queue |
|---|---|---|---|---|---|---|---|---|
| **Language** | Rust | TypeScript/Bun | TypeScript | Ruby | Python | Go | Go | Rust |
| **External deps** | None | Bun runtime | Redis | Redis | Redis/RabbitMQ | Cluster | None | Redis |
| **Single binary** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| **Distributed HA** | ✅ (Raft) | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ (Raft) | ❌ |
| **Job priorities** | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ✅ |
| **Delayed jobs** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ |
| **Cron scheduling** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **Retry + backoff** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ |
| **Dead Letter Queue** | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Job dependencies** | ✅ (DAG flows) | ✅ (basic) | ✅ (flows) | ❌ | ✅ (canvas) | ✅ | ❌ | ❌ |
| **Webhooks** | ✅ (HMAC signed) | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| **Rate limiting** | 🚧 (auth only) | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ |
| **Web dashboard** | ✅ (built-in) | ❌ | ❌ (paid) | ✅ | ✅ (Flower) | ✅ | ✅ | ❌ |
| **Prometheus** | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| **WebSocket events** | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Embeddable** | ✅ (lib) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (lib) |
| **Language-agnostic** | ✅ (HTTP/TCP) | Partial | ❌ | ❌ | ❌ | ✅ | ✅ | ❌ |
| **Memory footprint** | ~10-30 MB | ~80-150 MB | ~100+ MB | ~100+ MB | ~200+ MB | ~500+ MB | ~30-50 MB | ~30-50 MB |
| **License** | MIT / Apache-2.0 | MIT | MIT | Commercial | BSD | MIT | LGPL | MIT/Apache-2.0 |

### 5.2 Positioning Statement

RustQueue is positioned **between** the simplicity of cron/BullMQ and the power of Temporal:

```
Simplicity ◄─────────────────────────────────────────────► Power

  cron    BullMQ    Sidekiq   RustQueue    Dkron    Temporal    Airflow
   │        │         │        ▲            │         │           │
   │        │         │        │            │         │           │
   └────────┴─────────┴────────┴────────────┴─────────┴───────────┘
                               │
                     "Simple enough for a solo dev,
                      powerful enough for a platform team"
```

### 5.3 Key Differentiators

1. **Zero-dependency single binary** — Nothing else to install, configure, or manage.
2. **Distributed without infrastructure** — Raft consensus built-in; just run 3 copies.
3. **Embeddable** — Use as a standalone server OR as a Rust library.
4. **Language-agnostic** — HTTP REST + TCP protocol; works with any language.
5. **Smallest footprint** — Runs on a $5 VPS or a Raspberry Pi.
6. **Built-in dashboard** — No separate UI to deploy; open a browser and manage your queues.

---

## 6. Core Principles & Design Philosophy

### 6.1 Zero-Config Defaults, Full Configurability

RustQueue must work out of the box with `./rustqueue` and no configuration file. Every default should be production-reasonable. Advanced users can override anything via config file, environment variables, or CLI flags (in that priority order).

### 6.2 Batteries Included, Not Batteries Required

The dashboard, storage, metrics, and scheduling engine are all built-in. But users should be able to swap components: use Postgres instead of embedded storage, export metrics to Datadog instead of Prometheus, or skip the dashboard entirely.

### 6.3 Protocol-First, SDK-Second

The TCP and HTTP protocols are the primary interfaces. SDKs are thin wrappers for convenience. This ensures any language can integrate without waiting for an official SDK.

### 6.4 Crash-Only Design

RustQueue should be safe to kill at any time (`kill -9`). On restart, it must recover all state from durable storage with zero data loss. No graceful shutdown should ever be required for correctness.

### 6.5 Observability is Not Optional

Every job has a full lifecycle trace. Every queue has metrics. Every failure has context. If something goes wrong, the operator should be able to diagnose it without SSH-ing into the server.

### 6.6 Progressive Complexity

A solo developer running on a single VPS should never encounter distributed systems concepts. A platform team running a 10-node cluster should have all the knobs they need. The product should grow with the user.

---

## 7. Architecture

### 7.1 High-Level Architecture

```
                     ┌──────────────────────────────────────────────┐
                     │              RustQueue Binary                │
                     │                                              │
  Producers ────────►│  ┌────────────┐    ┌────────────────────┐   │
  (any language)     │  │ HTTP/REST  │    │   TCP Protocol     │   │
                     │  │  (axum)    │    │  (tokio raw TCP)   │   │
  Workers ◄─────────►│  │ + WebSocket│    │  + SSE             │   │
  (any language)     │  └─────┬──────┘    └────────┬───────────┘   │
                     │        │                    │               │
  Browser ──────────►│        └────────┬───────────┘               │
  (Dashboard)        │                 │                           │
                     │        ┌────────▼─────────┐                 │
                     │        │   Core Engine     │                 │
                     │        │                   │                 │
                     │        │  ┌─────────────┐  │                 │
                     │        │  │ Queue Mgr   │  │                 │
                     │        │  ├─────────────┤  │                 │
                     │        │  │ Scheduler   │  │                 │
                     │        │  ├─────────────┤  │                 │
                     │        │  │ Worker Mgr  │  │                 │
                     │        │  ├─────────────┤  │                 │
                     │        │  │ Flow Engine │  │                 │
                     │        │  ├─────────────┤  │                 │
                     │        │  │ DLQ Manager │  │                 │
                     │        │  ├─────────────┤  │                 │
                     │        │  │ Rate Limiter│  │                 │
                     │        │  └─────────────┘  │                 │
                     │        └────────┬──────────┘                 │
                     │                 │                            │
                     │        ┌────────▼──────────┐                │
                     │        │  Storage Layer    │                 │
                     │        │  (trait-based)    │                 │
                     │        │                   │                 │
                     │        │  ┌──────┐ ┌─────┐│                 │
                     │        │  │ redb │ │SQLite││                 │
                     │        │  └──────┘ └─────┘│                 │
                     │        │  ┌────────────┐  │                 │
                     │        │  │ PostgreSQL │  │                 │
                     │        │  └────────────┘  │                 │
                     │        └──────────────────┘                 │
                     │                                              │
                     │  ┌──────────────────────────────────────┐   │
                     │  │  Raft Consensus (optional cluster)   │   │
                     │  │  (openraft)                          │   │
                     │  └──────────────────────────────────────┘   │
                     │                                              │
                     │  ┌──────────────────────────────────────┐   │
                     │  │  Embedded Web Dashboard              │   │
                     │  │  (rust-embed + static HTML/JS)       │   │
                     │  └──────────────────────────────────────┘   │
                     └──────────────────────────────────────────────┘
```

### 7.2 Core Engine Components

#### 7.2.1 Queue Manager

Manages named queues and their configuration. Handles job insertion, dequeuing, and state transitions. Supports multiple ordering strategies per queue.

**Ordering strategies:**
- **FIFO** (default): First-in, first-out
- **LIFO**: Last-in, first-out
- **Priority**: Highest priority value dequeued first, with FIFO as tiebreaker
- **Fair**: Round-robin across job groups (for multi-tenant workloads) — 📋 Roadmap: enum exists, no round-robin logic

#### 7.2.2 Scheduler

Manages time-based job execution. Evaluates cron expressions and interval-based schedules on a configurable tick interval (default: 1 second). Creates jobs in the appropriate queue when their scheduled time arrives.

**Supported schedule types:**
- Standard cron expressions (5 fields: minute, hour, day, month, weekday)
- Extended cron expressions (6 fields: + seconds)
- Fixed interval (`every: 300000` = every 5 minutes)
- One-shot delayed execution (`run_at: "2026-03-01T00:00:00Z"`)

#### 7.2.3 Worker Manager — 🚧 Partial (heartbeat implemented, no worker registry)

Tracks registered workers, their heartbeats, assigned jobs, and capacity. Detects stalled workers (missing heartbeats) and reassigns their active jobs.

**Worker lifecycle:**
1. Worker connects and registers (protocol, queue, concurrency)
2. Worker pulls jobs (or server pushes, depending on mode)
3. Worker sends heartbeats while processing
4. Worker acknowledges completion or reports failure
5. If heartbeat is missed for `stall_timeout` (default: 30s), job is returned to queue

#### 7.2.4 Flow Engine — ✅ Implemented

Manages directed acyclic graphs (DAGs) of job dependencies. A child job starts in `Blocked` state until all its parent dependencies complete successfully. On parent `ack()`, children are promoted inline (not on scheduler tick) for minimal latency.

**DAG rules:**
- Cycles are detected at submission time via BFS and rejected
- If a parent goes to DLQ (exhausts retries), all Blocked children are recursively cascaded to DLQ
- If all dependencies are already Completed when a child is pushed, it goes directly to Waiting
- Maximum DAG depth: configurable (`max_dag_depth`, default: 10)
- In-memory reverse index (`DashMap<JobId, Vec<JobId>>`) for O(1) child lookup
- Scheduler safety net: every 10 ticks, promotes orphaned Blocked jobs whose deps are all Completed
- Flow status endpoint: `GET /api/v1/flows/{flow_id}` returns all jobs + summary counts

#### 7.2.5 DLQ Manager

Manages jobs that have exhausted their retry budget. Dead-lettered jobs are preserved with their full execution history (every attempt, every error message, every timestamp) for inspection and manual retry.

#### 7.2.6 Rate Limiter — 📋 Roadmap (auth rate limiter exists, no per-queue token bucket)

Implements per-queue rate limiting using a token bucket algorithm. Configurable as:
- **Max concurrent:** Maximum jobs executing simultaneously in a queue
- **Max rate:** Maximum jobs dequeued per time window (e.g., 100/minute)
- **Max per group:** Maximum concurrent jobs sharing a `group_id`

### 7.3 Storage Layer

Storage is abstracted behind a trait (`StorageBackend`) to allow multiple implementations:

```rust
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    // Completion operations (atomic, backend-aware)
    async fn complete_job(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<CompleteJobOutcome>;
    async fn complete_jobs_batch(
        &self,
        items: &[(JobId, Option<serde_json::Value>)],
    ) -> Result<Vec<CompleteJobOutcome>>;

    // Job operations
    async fn insert_job(&self, job: &Job) -> Result<JobId>;
    async fn insert_jobs_batch(&self, jobs: &[Job]) -> Result<Vec<JobId>>;
    async fn get_job(&self, id: JobId) -> Result<Option<Job>>;
    async fn update_job(&self, job: &Job) -> Result<()>;
    async fn delete_job(&self, id: JobId) -> Result<()>;
    
    // Queue operations
    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>>;
    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts>;
    
    // Scheduled jobs
    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>>;
    
    // DLQ
    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()>;
    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>>;
    
    // Cleanup
    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64>;
    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64>;
    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64>;
    
    // Cron schedules
    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()>;
    async fn get_active_schedules(&self) -> Result<Vec<Schedule>>;
    async fn delete_schedule(&self, name: &str) -> Result<()>;
    async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>>;
    async fn list_all_schedules(&self) -> Result<Vec<Schedule>>;

    // Discovery
    async fn list_queue_names(&self) -> Result<Vec<String>>;
    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>>;
    async fn get_active_jobs(&self) -> Result<Vec<Job>>;

    // Flows
    async fn get_jobs_by_flow_id(&self, flow_id: &str) -> Result<Vec<Job>>;
}
```

**Implementations (prioritized):**

| Backend | Priority | Use Case |
|---|---|---|
| `redb` | P0 (default) | Zero-config embedded, pure Rust, ACID |
| `sqlite` | P1 | Familiar, tooling ecosystem, debugging ease |
| `postgres` | P2 | Teams with existing Postgres, shared state — 🚧 Partial: config + trait defined, `main.rs` returns error at runtime |

### 7.4 Distributed Mode (Cluster)

When running in cluster mode (3+ nodes), RustQueue uses the Raft consensus protocol to replicate state across nodes:

- **Leader** handles all write operations (PUSH, ACK, FAIL, schedule changes)
- **Followers** serve read operations (GET, STATS, PULL in read-only mode) and replicate the leader's log
- **Leader election** is automatic; if the leader dies, a follower takes over within seconds
- **Job execution** only happens on the leader to prevent duplicate processing
- **Split-brain protection** via Raft quorum (requires majority of nodes to agree)

**Cluster discovery:**
- Static: Provide a list of peer addresses in config
- DNS: Resolve a DNS name to discover peers (for Kubernetes/Consul integration)

---

## 8. Functional Requirements

### 8.1 Job Lifecycle

A job progresses through the following states:

```
                         ┌──────────┐
                         │ CREATED  │
                         └────┬─────┘
                              │
                    ┌─────────▼──────────┐
              ┌─────┤    WAITING / DELAYED├─────┐
              │     └─────────┬──────────┘      │
              │               │                 │
              │     ┌─────────▼─────────┐       │
              │     │      ACTIVE       │       │
              │     └───┬──────────┬────┘       │
              │         │          │            │
              │  ┌──────▼───┐ ┌───▼────────┐   │
              │  │COMPLETED │ │  FAILED     │   │
              │  └──────────┘ └───┬────────┘   │
              │                   │            │
              │         ┌─────────▼─────────┐  │
              │         │  retries left?    │  │
              │         │  yes → WAITING    │──┘
              │         │  no  → DLQ        │
              │         └───────────────────┘
              │
         ┌────▼─────┐
         │ CANCELLED │
         └──────────┘
```

**State definitions:**

| State | Description |
|---|---|
| `created` | Job has been submitted but not yet enqueued (used in flows waiting for children) |
| `waiting` | Job is in the queue, eligible for execution |
| `delayed` | Job is waiting for its scheduled time to arrive |
| `active` | Job has been assigned to a worker and is being processed |
| `completed` | Job finished successfully |
| `failed` | Job's current attempt failed; may be retried |
| `dlq` | Job exhausted all retries and is in the dead letter queue |
| `cancelled` | Job was cancelled before completion |
| `blocked` | Job is waiting for dependent jobs (children in a flow) |

### 8.2 Job Operations

#### P0 — Must Have (v0.1)

| ID | Requirement | Description |
|---|---|---|
| JOB-01 | Push job | Submit a job to a named queue with a JSON payload |
| JOB-02 | Pull job | Dequeue the next eligible job from a named queue |
| JOB-03 | Acknowledge | Mark an active job as completed with an optional result |
| JOB-04 | Fail | Mark an active job as failed with an error message |
| JOB-05 | Get job | Retrieve a job by ID, including full state and history |
| JOB-06 | Cancel job | Cancel a waiting or delayed job |
| JOB-07 | Batch push | Submit multiple jobs in a single request |
| JOB-08 | Batch pull | Dequeue multiple jobs in a single request |
| JOB-09 | Batch ack | Acknowledge multiple jobs in a single request |
| JOB-10 | Job priority | Higher priority jobs are dequeued first |
| JOB-11 | Delayed jobs | Jobs can specify a delay (ms) or absolute run time |
| JOB-12 | TTL | Jobs expire and are discarded if not processed within TTL |
| JOB-13 | Unique jobs | Deduplication via a user-provided unique key per queue |

#### P1 — Should Have (v0.2)

| ID | Requirement | Description |
|---|---|---|
| JOB-14 | Retry with backoff | Automatic retries with configurable backoff (fixed, linear, exponential) |
| JOB-15 | Max attempts | Configurable maximum retry count per job (default: 3) |
| JOB-16 | Dead letter queue | Failed jobs are moved to DLQ after exhausting retries |
| JOB-17 | DLQ inspection | List, inspect, and retry DLQ jobs via API |
| JOB-18 | DLQ purge | Bulk delete DLQ jobs by queue or age |
| JOB-19 | Progress tracking | Workers can report progress (0-100%) on active jobs |
| JOB-20 | Job logs | Workers can append log entries to a job's log |
| JOB-21 | Job heartbeat | Workers send heartbeats; stalled jobs are reclaimed |
| JOB-22 | Job timeout | Jobs are failed if processing exceeds a timeout |
| JOB-23 | Job tags | Arbitrary string tags for filtering and grouping |
| JOB-24 | Job groups | Group ID for fair scheduling and per-group rate limiting |

#### P2 — Nice to Have (v0.3+)

| ID | Requirement | Description |
|---|---|---|
| JOB-25 | Job dependencies | Parent jobs wait for children to complete (DAG flows) |
| JOB-26 | Promote | Move a delayed job to waiting immediately |
| JOB-27 | Change priority | Update the priority of a waiting job |
| JOB-28 | Job data update | Update the payload of a waiting job |
| JOB-29 | Remove on complete | Optionally auto-delete jobs after completion |
| JOB-30 | Remove on fail | Optionally auto-delete jobs after final failure |
| JOB-31 | Job result storage | Store and retrieve the result of completed jobs |

### 8.3 Queue Operations

| ID | Priority | Requirement | Description |
|---|---|---|---|
| Q-01 | P0 | List queues | List all known queues with summary statistics |
| Q-02 | P0 | Queue stats | Get counts by state (waiting, active, delayed, completed, failed, dlq) |
| Q-03 | P1 | Pause queue | Stop dequeuing from a queue (jobs still accepted) |
| Q-04 | P1 | Resume queue | Re-enable dequeuing from a paused queue |
| Q-05 | P1 | Drain queue | Remove all waiting jobs from a queue |
| Q-06 | P2 | Obliterate queue | Remove all data associated with a queue |
| Q-07 | P1 | Clean queue | Remove completed/failed jobs older than a threshold |
| Q-08 | P1 | Rate limit | Set max jobs/second and max concurrency per queue — 📋 Roadmap |
| Q-09 | P2 | Queue ordering | Configure FIFO, LIFO, priority, or fair ordering per queue |

### 8.4 Scheduling (Cron)

| ID | Priority | Requirement | Description |
|---|---|---|---|
| CRON-01 | P1 | Create schedule | Define a recurring schedule with cron expression or interval |
| CRON-02 | P1 | Upsert schedule | Create or update a schedule by name (idempotent) |
| CRON-03 | P1 | Delete schedule | Remove a schedule by name |
| CRON-04 | P1 | List schedules | List all active schedules with next run time |
| CRON-05 | P1 | Pause schedule | Temporarily disable a schedule without deleting it |
| CRON-06 | P2 | Max executions | Limit total number of times a schedule fires |
| CRON-07 | P2 | Timezone support | Schedules can specify a timezone (default: UTC) |
| CRON-08 | P2 | Backfill | Retroactively create jobs for missed schedule windows |

### 8.5 Webhooks — ✅ Implemented (v0.13)

| ID | Priority | Requirement | Description |
|---|---|---|---|
| WH-01 | P2 | Register webhook | ✅ `POST /api/v1/webhooks` with URL, events, queues, secret |
| WH-02 | P2 | Event filtering | ✅ Filter by event type (JobPushed/Completed/Failed/Dlq/Cancelled/Progress) and queue |
| WH-03 | P2 | Retry delivery | ✅ 3 retries with exponential backoff (1s, 2s, 4s) |
| WH-04 | P2 | Webhook signing | ✅ HMAC-SHA256 via `hmac` + `sha2` crates, `X-RustQueue-Signature` header |
| WH-05 | P2 | Webhook management | ✅ List, get, delete via HTTP API + TCP commands |

### 8.6 Worker Management — 🚧 Partial (heartbeat + stall detection implemented)

| ID | Priority | Requirement | Description |
|---|---|---|---|
| WRK-01 | P1 | Register worker | Workers register with name, queue, and concurrency |
| WRK-02 | P1 | Unregister worker | Clean disconnection, reassign active jobs |
| WRK-03 | P1 | List workers | Show active workers with their assigned jobs |
| WRK-04 | P1 | Stall detection | Detect workers that stopped sending heartbeats |
| WRK-05 | P2 | Worker groups | Tag workers for targeted job routing |

---

## 9. Non-Functional Requirements

### 9.1 Performance

| Metric | Target | Rationale |
|---|---|---|
| Throughput (push) | ≥ 50,000 jobs/sec (single node) | Competitive with Redis-based solutions |
| Throughput (push+ack) | ≥ 30,000 jobs/sec round-trip | Real-world workload simulation |
| Latency (p50 push) | < 1 ms | Near-instantaneous for most use cases |
| Latency (p99 push) | < 5 ms | Tail latency matters for SLA-sensitive workloads |
| Scheduling precision | ± 1 second | Acceptable for cron-style workloads |
| Startup time | < 500 ms (cold start) | Fast restarts after crashes or deployments |
| Recovery time | < 5 seconds | Time to full operation after crash (WAL replay) |

Current benchmark snapshots (February 7, 2026) used for phase tracking:

- **RustQueue beats RabbitMQ** on produce (1.2x), consume (5.3x), and end-to-end (5.1x)
- Hybrid TCP produce (batch_size=50): **~43,494 push/sec** (vs RabbitMQ 35,975)
- Hybrid TCP consume (batch_size=50): **~14,195 ops/sec** (vs RabbitMQ 2,675)
- Hybrid TCP end-to-end (batch_size=50): **~14,681 ops/sec** (vs RabbitMQ 2,902)
- Hybrid TCP produce (batch_size=1): **~23,382 push/sec**
- Sequential single-job redb (no coalescing): ~348 push/sec
- Concurrent push with write coalescing (100 callers): **~22,222 push/sec** (60.6x improvement)
- References: `docs/performance-analysis.md`, `docs/competitor-benchmark-2026-02-07.md`

### 9.2 Reliability

| Requirement | Target |
|---|---|
| Durability | Zero job loss on crash (all accepted jobs are persisted before acknowledgment) |
| Exactly-once delivery | At-least-once guaranteed; exactly-once via unique keys + idempotent workers |
| Cluster failover | < 10 seconds leader election in Raft mode |
| Data integrity | ACID transactions for all state transitions |

### 9.3 Scalability

| Dimension | Target |
|---|---|
| Queues | 10,000+ named queues per instance |
| Jobs in queue | 10,000,000+ per queue |
| Connected workers | 10,000+ simultaneous TCP connections |
| Cluster size | 3-7 nodes (Raft limitation) |

### 9.4 Resource Usage

| Resource | Target (idle) | Target (1K jobs/sec) |
|---|---|---|
| Memory | < 20 MB | < 100 MB |
| CPU | < 1% of one core | < 25% of one core |
| Disk (binary) | < 15 MB | — |
| Disk (storage) | ~100 bytes/job | ~100 bytes/job |

### 9.5 Compatibility

| Platform | Support Level |
|---|---|
| Linux x86_64 | Tier 1 (prebuilt binaries, CI tested) |
| Linux ARM64 | Tier 1 |
| macOS x86_64 | Tier 1 |
| macOS ARM64 (Apple Silicon) | Tier 1 |
| Windows x86_64 | Tier 2 (prebuilt binaries, community tested) |
| FreeBSD | Tier 3 (builds from source) |

---

## 10. Data Model

### 10.1 Job

```rust
pub struct Job {
    // Identity
    pub id: JobId,                          // Auto-generated UUID v7 (time-sortable)
    pub custom_id: Option<String>,          // User-provided identifier
    pub name: String,                       // Job type name (e.g., "send-email")
    pub queue: String,                      // Queue name
    
    // Payload
    pub data: serde_json::Value,            // Arbitrary JSON payload
    pub result: Option<serde_json::Value>,  // Result from worker on completion
    
    // State
    pub state: JobState,                    // Current lifecycle state
    pub progress: Option<u8>,              // 0-100 progress percentage
    pub logs: Vec<LogEntry>,               // Append-only log entries
    
    // Scheduling
    pub priority: i32,                      // Higher = processed first (default: 0)
    pub delay_until: Option<DateTime<Utc>>, // Don't process before this time
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    
    // Retry configuration
    pub max_attempts: u32,                  // Max retries (default: 3)
    pub attempt: u32,                       // Current attempt number
    pub backoff: BackoffStrategy,           // Fixed, linear, or exponential
    pub backoff_delay_ms: u64,              // Base delay for backoff
    pub last_error: Option<String>,         // Most recent error message
    
    // Constraints
    pub ttl_ms: Option<u64>,               // Time-to-live from creation
    pub timeout_ms: Option<u64>,           // Max processing time per attempt
    pub unique_key: Option<String>,        // Deduplication key (unique per queue)
    
    // Organization
    pub tags: Vec<String>,                 // Arbitrary tags for filtering
    pub group_id: Option<String>,          // Group for fair scheduling / rate limiting
    
    // Dependencies
    pub depends_on: Vec<JobId>,            // Parent jobs that must complete first
    pub flow_id: Option<String>,           // ID of the flow/DAG this job belongs to
    
    // Behavior flags
    pub lifo: bool,                        // Last-in-first-out ordering
    pub remove_on_complete: bool,          // Auto-delete after completion
    pub remove_on_fail: bool,              // Auto-delete after final failure
    
    // Worker assignment
    pub worker_id: Option<String>,         // ID of the worker processing this job
    pub last_heartbeat: Option<DateTime<Utc>>,
}
```

### 10.2 Schedule (Cron)

```rust
pub struct Schedule {
    pub name: String,                      // Unique schedule identifier
    pub queue: String,                     // Target queue for generated jobs
    pub job_name: String,                  // Name for generated jobs
    pub job_data: serde_json::Value,       // Payload for generated jobs
    
    // Timing
    pub cron_expr: Option<String>,         // Cron expression (mutually exclusive with every_ms)
    pub every_ms: Option<u64>,             // Fixed interval in milliseconds
    pub timezone: Option<String>,          // IANA timezone (default: UTC)
    
    // Constraints
    pub max_executions: Option<u64>,       // Stop after N executions
    pub execution_count: u64,              // How many times it has fired
    pub paused: bool,                      // Temporarily disabled
    
    // Job options (applied to generated jobs)
    pub job_options: JobOptions,           // Priority, retries, timeout, etc.
    
    // Metadata
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### 10.3 Worker

```rust
pub struct Worker {
    pub id: String,                        // Unique worker identifier
    pub name: Option<String>,              // Human-readable name
    pub queues: Vec<String>,               // Queues this worker consumes from
    pub concurrency: u32,                  // Max simultaneous jobs
    pub active_jobs: Vec<JobId>,           // Currently processing
    pub tags: Vec<String>,                 // For targeted routing
    
    pub connected_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub jobs_completed: u64,               // Lifetime stats
    pub jobs_failed: u64,
}
```

### 10.4 Webhook

```rust
pub struct Webhook {
    pub id: String,
    pub url: String,                       // HTTP endpoint to call
    pub events: Vec<WebhookEvent>,         // Subscribed event types
    pub queues: Option<Vec<String>>,       // Filter by queue (None = all)
    pub secret: Option<String>,            // HMAC signing secret
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

pub enum WebhookEvent {
    JobCompleted,
    JobFailed,
    JobDlq,
    JobProgress,
    QueuePaused,
    QueueResumed,
    WorkerConnected,
    WorkerDisconnected,
    ScheduleFired,
}
```

### 10.5 Enumerations

```rust
pub enum JobState {
    Created,
    Waiting,
    Delayed,
    Active,
    Completed,
    Failed,
    Dlq,
    Cancelled,
    Blocked,
}

pub enum BackoffStrategy {
    Fixed,          // Same delay every retry
    Linear,         // delay * attempt
    Exponential,    // delay * 2^attempt
}

pub enum QueueOrdering {
    Fifo,
    Lifo,
    Priority,
    Fair,
}
```

---

## 11. API Design

### 11.1 Protocols

RustQueue exposes two protocols:

| Protocol | Port (default) | Use Case |
|---|---|---|
| **HTTP/REST** | 6790 | General use, browser dashboard, webhooks, compatibility |
| **TCP** | 6789 | High-throughput worker connections, low-latency operations |

Both protocols support the same operations. The TCP protocol uses newline-delimited JSON for simplicity and performance.

### 11.2 HTTP REST API

#### Jobs

```
POST   /api/v1/queues/{queue}/jobs          # Push job(s)
GET    /api/v1/queues/{queue}/jobs           # Pull next job (long-poll with ?timeout=)
GET    /api/v1/jobs/{id}                     # Get job by ID
POST   /api/v1/jobs/{id}/ack                # Acknowledge completion
POST   /api/v1/jobs/{id}/fail               # Report failure
POST   /api/v1/jobs/{id}/cancel             # Cancel job
POST   /api/v1/jobs/{id}/progress           # Update progress
POST   /api/v1/jobs/{id}/log                # Append log entry
POST   /api/v1/jobs/{id}/heartbeat          # Send heartbeat
PUT    /api/v1/jobs/{id}/priority            # Change priority
POST   /api/v1/jobs/{id}/promote            # Move delayed → waiting
GET    /api/v1/jobs/{id}/logs               # Get job logs
GET    /api/v1/jobs/{id}/result             # Get job result
```

#### Queues

```
GET    /api/v1/queues                       # List all queues
GET    /api/v1/queues/{queue}/stats         # Queue statistics
POST   /api/v1/queues/{queue}/pause         # Pause queue
POST   /api/v1/queues/{queue}/resume        # Resume queue
DELETE /api/v1/queues/{queue}/drain         # Remove waiting jobs
DELETE /api/v1/queues/{queue}               # Obliterate queue
POST   /api/v1/queues/{queue}/clean         # Clean old jobs
PUT    /api/v1/queues/{queue}/rate-limit    # Set rate limit
PUT    /api/v1/queues/{queue}/concurrency   # Set concurrency limit
```

#### Dead Letter Queue

```
GET    /api/v1/queues/{queue}/dlq           # List DLQ jobs
POST   /api/v1/queues/{queue}/dlq/retry     # Retry DLQ jobs
DELETE /api/v1/queues/{queue}/dlq           # Purge DLQ
```

#### Schedules

```
POST   /api/v1/schedules                    # Create schedule
PUT    /api/v1/schedules/{name}             # Upsert schedule
GET    /api/v1/schedules                    # List schedules
GET    /api/v1/schedules/{name}             # Get schedule
DELETE /api/v1/schedules/{name}             # Delete schedule
POST   /api/v1/schedules/{name}/pause       # Pause schedule
POST   /api/v1/schedules/{name}/resume      # Resume schedule
```

#### Flows

```
POST   /api/v1/flows                        # Create flow (DAG of jobs)
GET    /api/v1/flows/{id}                   # Get flow status
```

#### Webhooks

```
POST   /api/v1/webhooks                     # Register webhook
GET    /api/v1/webhooks                     # List webhooks
DELETE /api/v1/webhooks/{id}                # Remove webhook
```

#### Workers

```
GET    /api/v1/workers                      # List connected workers
```

#### Monitoring

```
GET    /api/v1/stats                        # Global statistics
GET    /api/v1/health                       # Health check
GET    /api/v1/metrics/prometheus            # Prometheus metrics
```

#### Real-time

```
WS     /api/v1/ws                           # WebSocket (all events)
WS     /api/v1/ws/queues/{queue}            # WebSocket (queue-specific)
GET    /api/v1/events                       # SSE (all events)
GET    /api/v1/events/queues/{queue}        # SSE (queue-specific)
```

### 11.3 TCP Protocol

Newline-delimited JSON. Each message is a single JSON object followed by `\n`.

```json
// Authentication (if enabled)
→ {"cmd":"auth","token":"secret123"}
← {"ok":true}

// Push a job
→ {"cmd":"push","queue":"emails","name":"send-welcome","data":{"to":"a@b.com"},"priority":5}
← {"ok":true,"id":"01JKXYZ..."}

// Pull a job
→ {"cmd":"pull","queue":"emails","timeout":30000}
← {"ok":true,"job":{"id":"01JKXYZ...","name":"send-welcome","data":{"to":"a@b.com"}}}

// Acknowledge
→ {"cmd":"ack","id":"01JKXYZ...","result":{"sent":true}}
← {"ok":true}

// Fail
→ {"cmd":"fail","id":"01JKXYZ...","error":"SMTP timeout"}
← {"ok":true,"retry":true,"next_attempt_at":"2026-02-05T12:05:00Z"}

// Subscribe to events
→ {"cmd":"subscribe","queues":["emails","reports"]}
← {"event":"job:completed","queue":"emails","job_id":"01JKXYZ..."}
← {"event":"job:failed","queue":"reports","job_id":"01JKABC..."}
```

### 11.4 Error Responses

All errors follow a consistent format:

```json
{
  "ok": false,
  "error": {
    "code": "QUEUE_NOT_FOUND",
    "message": "Queue 'nonexistent' does not exist",
    "details": null
  }
}
```

**Standard error codes:**

| Code | HTTP Status | Description |
|---|---|---|
| `QUEUE_NOT_FOUND` | 404 | Queue does not exist |
| `JOB_NOT_FOUND` | 404 | Job ID not found |
| `INVALID_STATE` | 409 | Job is not in the expected state for this operation |
| `DUPLICATE_KEY` | 409 | A job with this unique key already exists |
| `QUEUE_PAUSED` | 503 | Queue is paused, cannot dequeue |
| `RATE_LIMITED` | 429 | Rate limit exceeded |
| `UNAUTHORIZED` | 401 | Missing or invalid auth token |
| `VALIDATION_ERROR` | 400 | Invalid request payload |
| `INTERNAL_ERROR` | 500 | Unexpected server error |

---

## 12. SDK & Client Libraries

### 12.1 Strategy

RustQueue is **protocol-first**. The HTTP REST API and TCP protocol are the primary interfaces. SDKs are thin, ergonomic wrappers.

### 12.2 Official SDKs (by priority)

| SDK | Priority | Status | Location |
|---|---|---|---|
| **Rust** (embedded library) | P0 | ✅ Shipped | `src/builder.rs` — `RustQueue::memory()`, `::redb()`, `::hybrid()` |
| **TypeScript/JavaScript** | P0 | ✅ Shipped | `sdk/node/` — HTTP + TCP clients, zero deps, ESM/CJS |
| **Python** | P1 | ✅ Shipped | `sdk/python/` — HTTP client, stdlib-only, Python >= 3.8 |
| **Go** | P1 | ✅ Shipped | `sdk/go/` — HTTP + TCP clients, zero deps, Go >= 1.21 |
| **OpenAPI spec** | P0 | ✅ Shipped | `src/api/openapi.rs` — OpenAPI 3.1, Scalar UI at `/api/v1/docs` |

### 12.3 SDK Design Principles

- SDKs should be **thin wrappers** over the HTTP/TCP protocol
- SDKs should provide **type-safe** job definitions where the language supports it
- SDKs should handle **reconnection** and **heartbeats** automatically
- SDKs should be published to the language's standard package registry (npm, PyPI, crates.io, pkg.go.dev)

### 12.4 TypeScript SDK Example (Target API)

```typescript
import { Queue, Worker, FlowProducer } from '@rustqueue/client';

// Producer
const queue = new Queue('emails', { url: 'http://localhost:6790' });
await queue.add('send-welcome', { to: 'user@example.com' }, {
  priority: 10,
  delay: 5000,
  attempts: 3,
  backoff: { type: 'exponential', delay: 1000 },
});

// Worker
const worker = new Worker('emails', async (job) => {
  await job.updateProgress(50);
  await sendEmail(job.data.to);
  return { sent: true };
}, {
  url: 'tcp://localhost:6789',
  concurrency: 5,
});

worker.on('completed', (job, result) => console.log('Done:', result));
worker.on('failed', (job, err) => console.error('Failed:', err));
```

---

## 13. Deployment & Configuration

### 13.1 Installation Methods

```bash
# 1. Pre-built binary (Linux/macOS)
curl -sSL https://rustqueue.dev/install.sh | sh

# 2. Homebrew (macOS)
brew install rustqueue

# 3. Cargo (from source)
cargo install rustqueue

# 4. Docker
docker run -p 6789:6789 -p 6790:6790 ghcr.io/rustqueue/rustqueue

# 5. Docker Compose (see below)
```

### 13.2 Configuration

Configuration is resolved in order: **CLI flags > environment variables > config file > defaults**.

#### Config File (`rustqueue.toml`)

```toml
[server]
host = "0.0.0.0"
tcp_port = 6789
http_port = 6790

[storage]
backend = "redb"                  # "redb", "hybrid", "in_memory", "sqlite", "postgres"
path = "/var/lib/rustqueue/data"  # For redb/sqlite/hybrid
redb_durability = "immediate"     # "none" (unsafe fastest), "eventual", "immediate" (safest)
write_coalescing_enabled = false  # Buffer single push/ack into batched flushes
write_coalescing_interval_ms = 10 # Flush interval (ms)
write_coalescing_max_batch = 100  # Max buffered ops before forced flush
hybrid_snapshot_interval_ms = 1000 # Hybrid: how often to flush to disk (ms)
hybrid_max_dirty = 5000           # Hybrid: max dirty entries before forced flush
# postgres_url = "postgres://..."  # For postgres backend

[auth]
enabled = false
tokens = ["token1", "token2"]     # Static bearer tokens

[scheduler]
tick_interval_ms = 1000           # How often to check for ready scheduled jobs
stall_check_interval_ms = 5000    # How often to check for stalled workers

[jobs]
default_max_attempts = 3
default_backoff = "exponential"
default_backoff_delay_ms = 1000
default_timeout_ms = 300000       # 5 minutes
stall_timeout_ms = 30000          # 30 seconds

[retention]
completed_ttl = "7d"              # Auto-clean completed jobs after 7 days
failed_ttl = "30d"                # Auto-clean failed jobs after 30 days
dlq_ttl = "90d"                   # Auto-clean DLQ after 90 days

[dashboard]
enabled = true
path_prefix = "/dashboard"

[cluster]
enabled = false
node_id = "node-1"
peers = ["10.0.0.2:6800", "10.0.0.3:6800"]
raft_port = 6800

[logging]
level = "info"                    # trace, debug, info, warn, error
format = "json"                   # "json" or "pretty"

[metrics]
prometheus_enabled = true
prometheus_path = "/api/v1/metrics/prometheus"
```

#### Environment Variables

All config values map to environment variables with the `RUSTQUEUE_` prefix:

```bash
RUSTQUEUE_TCP_PORT=6789
RUSTQUEUE_HTTP_PORT=6790
RUSTQUEUE_STORAGE_BACKEND=redb
RUSTQUEUE_STORAGE_PATH=/var/lib/rustqueue/data
RUSTQUEUE_AUTH_TOKENS=token1,token2
RUSTQUEUE_CLUSTER_ENABLED=true
RUSTQUEUE_LOG_LEVEL=info
```

### 13.3 Docker Compose

RustQueue ships with ready-to-use Docker deployment files:

```bash
# Standalone deployment (builds from source)
docker compose up -d

# Full monitoring stack (RustQueue + Prometheus + Grafana)
docker compose -f docker-compose.monitoring.yml up -d

# Access points:
#   RustQueue HTTP API:  http://localhost:6790
#   RustQueue Dashboard: http://localhost:6790/dashboard
#   Prometheus:          http://localhost:9090
#   Grafana:             http://localhost:3000 (admin/admin)
```

**Deployment files:**

| File | Purpose |
|------|---------|
| `Dockerfile` | Multi-stage build (rust:1.85-slim → debian:bookworm-slim), non-root user |
| `docker-compose.yml` | Standalone RustQueue with persistent volume |
| `docker-compose.monitoring.yml` | RustQueue + Prometheus + Grafana (auto-provisioned dashboard) |
| `deploy/rustqueue.toml` | Production config template |
| `deploy/prometheus.yml` | Prometheus scrape config for RustQueue metrics |
| `deploy/grafana-datasources.yml` | Grafana Prometheus datasource provisioning |
| `deploy/grafana-dashboards.yml` | Grafana dashboard provisioning |

### 13.4 Docker Compose (3-Node Cluster) — Future

Cluster mode requires the distributed Raft consensus feature (Phase 7). Multi-node Docker Compose examples will be provided when cluster mode ships.

---

## 14. Security

### 14.1 Authentication

#### Phase 1: Static Token Auth (v0.1)

- Bearer tokens configured via environment variable or config file
- Tokens are validated on every request (HTTP `Authorization: Bearer <token>`, TCP `auth` command)
- No token = open access (suitable for development and single-tenant deployments)

#### Phase 2: API Keys with Scopes (v0.4+)

- Generate API keys with specific permissions (read-only, write, admin)
- Keys are stored hashed in the database
- Each key has an associated set of allowed queues and operations

#### Phase 3: SSO / OIDC (v1.0+ / Pro)

- OpenID Connect integration for the web dashboard
- Role-based access control (viewer, operator, admin)

### 14.2 Transport Security

- **TLS** support for both HTTP and TCP protocols via `rustls` (no OpenSSL dependency)
- TLS is optional but strongly recommended for production
- Certificate can be provided via file path or auto-generated with Let's Encrypt (stretch goal)

### 14.3 Data Security

- Job payloads are stored as-is (plaintext JSON). Users are responsible for encrypting sensitive data before submission.
- Auth tokens are never logged or exposed via the API.
- The dashboard requires authentication when auth is enabled.

### 14.4 Webhook Security

- Webhook payloads are signed with HMAC-SHA256 using a per-webhook secret
- Signature is included in the `X-RustQueue-Signature` header
- Receiving services should verify the signature before processing

---

## 15. Observability & Monitoring

### 15.1 Structured Logging

All logs are structured JSON by default (with a `pretty` mode for development):

```json
{
  "timestamp": "2026-02-05T12:00:00.123Z",
  "level": "info",
  "target": "rustqueue::engine",
  "message": "Job completed",
  "job_id": "01JKXYZ...",
  "queue": "emails",
  "name": "send-welcome",
  "duration_ms": 234,
  "attempt": 1
}
```

Log levels: `trace`, `debug`, `info`, `warn`, `error`.

### 15.2 Prometheus Metrics

Exposed at `GET /api/v1/metrics/prometheus`. Key metrics:

**Counters (shipped):**
- `rustqueue_jobs_pushed_total{queue}` — Total jobs submitted
- `rustqueue_jobs_completed_total{queue}` — Total jobs completed
- `rustqueue_jobs_failed_total{queue}` — Total job failures (including retries)
- `rustqueue_jobs_pulled_total{queue}` — Total jobs pulled by workers
- `rustqueue_http_requests_total{method, status}` — HTTP request count by method and status code

**Gauges (shipped):**
- `rustqueue_queue_waiting_jobs{queue}` — Current waiting jobs
- `rustqueue_queue_active_jobs{queue}` — Current active jobs
- `rustqueue_queue_delayed_jobs{queue}` — Current delayed jobs
- `rustqueue_queue_dlq_jobs{queue}` — Current DLQ size
- `rustqueue_websocket_clients_connected` — Connected WebSocket clients
- `rustqueue_scheduler_tick_duration_seconds` — Scheduler tick duration

**Histograms (shipped):**
- `rustqueue_push_duration_seconds` — Push operation latency
- `rustqueue_pull_duration_seconds` — Pull operation latency
- `rustqueue_ack_duration_seconds` — Ack operation latency
- `rustqueue_http_request_duration_seconds{method, status}` — HTTP request latency

**Planned:**
- `rustqueue_jobs_dlq_total{queue}` — Total jobs moved to DLQ
- `rustqueue_jobs_retried_total{queue}` — Total retry attempts
- `rustqueue_schedules_fired_total{schedule}` — Times each schedule has fired

A pre-built **Grafana dashboard** is provided at `docs/grafana/rustqueue-dashboard.json`.

### 15.3 Health Check

```
GET /api/v1/health

# Healthy response (200):
{ "ok": true, "status": "healthy", "version": "0.1.0", "uptime_seconds": 86400 }

# Degraded response (200):
{ "ok": true, "status": "degraded", "reason": "high_dlq_count", ... }

# Unhealthy response (503):
{ "ok": false, "status": "unhealthy", "reason": "storage_error", ... }
```

### 15.4 Real-Time Events

Events are emitted via WebSocket and SSE for real-time monitoring:

```json
{
  "event": "job:completed",
  "timestamp": "2026-02-05T12:00:00.123Z",
  "queue": "emails",
  "job_id": "01JKXYZ...",
  "job_name": "send-welcome",
  "duration_ms": 234,
  "result": { "sent": true }
}
```

**Event types:**
`job:waiting`, `job:active`, `job:completed`, `job:failed`, `job:dlq`, `job:progress`, `job:retrying`, `queue:paused`, `queue:resumed`, `worker:connected`, `worker:disconnected`, `worker:stalled`, `schedule:fired`

---

## 16. Web Dashboard

### 16.1 Overview

The dashboard is a static web application compiled into the RustQueue binary using `rust-embed`. No separate deployment required — opening `http://localhost:6790/dashboard` shows the UI.

### 16.2 Technology

- **Framework:** Vanilla JS + lightweight reactive library (Preact or Alpine.js) to minimize bundle size
- **Styling:** Tailwind CSS (purged for minimal size)
- **Target size:** < 500 KB total (HTML + JS + CSS)
- **Data:** All data fetched via the REST API; dashboard has no special access

### 16.3 Dashboard Pages

#### Overview Page
- Total jobs by state (waiting, active, delayed, completed, failed, DLQ) as cards
- Throughput chart (jobs/minute over the last hour)
- Active queues with summary stats
- Recent failures (last 10)

#### Queues Page
- List of all queues with: name, waiting count, active count, DLQ count, throughput
- Click a queue to see its jobs, filter by state
- Pause/resume buttons
- Rate limit configuration

#### Job Detail Page
- Full job data (payload, result, state, timestamps)
- Attempt history (each attempt's start time, duration, error)
- Log entries
- Progress bar (if applicable)
- Retry / cancel / promote actions

#### DLQ Page
- List of dead-lettered jobs across all queues
- Inspect individual failures
- Bulk retry or purge

#### Schedules Page
- List of cron schedules with next run time
- Create / edit / delete schedules
- Pause / resume individual schedules
- Execution history

#### Workers Page
- Connected workers with: name, queues, concurrency, active jobs, uptime
- Highlight stalled workers

#### Cluster Page (when cluster mode is enabled)
- Node list with role (leader/follower), status, last heartbeat
- Raft log status

---

## 17. Release Plan & Milestones

### Phase 1: Foundation (v0.1) — Weeks 1-6 ✅ COMPLETE

**Goal:** A working job queue with HTTP and TCP APIs.

| Deliverable | Status | Description |
|---|---|---|
| Core engine | ✅ | Queue manager, job state machine, priority ordering |
| redb storage | ✅ | Embedded storage backend with ACID transactions |
| HTTP API | ✅ | Push, pull, ack, fail, cancel, get job, list queues, stats |
| TCP protocol | ✅ | Same operations over newline-delimited JSON |
| CLI | ✅ | `rustqueue serve` with TOML + env + CLI config |
| Prometheus metrics | ✅ | Jobs pushed/completed/failed/pulled counters |
| Structured logging | ✅ | JSON and text output via tracing |
| Benchmarks | ✅ | Throughput benchmarks for push, pull, ack (criterion) |
| Tests | ✅ | Unit, integration, property-based tests, full lifecycle test |

**Exit criteria:** ✅ Core engine works end-to-end. Push, pull, ack with zero data loss on crash. Throughput with redb: ~340 push/sec (per-operation fsync). See `docs/performance-analysis.md` for optimization plan.

### Phase 2: Differentiation (v0.2) — Weeks 7-12 ✅ COMPLETE

**Goal:** Make RustQueue the most flexible job queue in Rust — multiple storage backends, embeddable library API, CLI management, and essential production features.

| Deliverable | Status | Description |
|---|---|---|
| In-Memory backend | ✅ | Always compiled, ideal for tests and ephemeral queues |
| SQLite backend | ✅ | WAL mode, `json_extract()` queries, behind `sqlite` feature |
| PostgreSQL backend | ✅ | `SELECT FOR UPDATE SKIP LOCKED`, behind `postgres` feature |
| Generic test harness | ✅ | `backend_tests!` macro — 19 canonical tests × N backends |
| Config-driven backend | ✅ | `storage.backend` in TOML selects redb/memory/sqlite/postgres |
| RustQueue builder | ✅ | `RustQueue::memory().build()` / `RustQueue::redb(path).build()` for embeddable use |
| Custom job IDs | ✅ | `JobOptions.custom_id` for idempotency |
| Job progress tracking | ✅ | `update_progress(id, 0-100, message)` via HTTP + TCP |
| Job timeouts | ✅ | Background scheduler detects and retries timed-out active jobs |
| Stall detection | ✅ | Configurable stall timeout, worker heartbeat support |
| Worker heartbeats | ✅ | `POST /jobs/{id}/heartbeat` + TCP `heartbeat` command |
| Delayed job promotion | ✅ | Scheduler promotes Delayed→Waiting when delay expires |
| CLI management | ✅ | `status`, `push`, `inspect` subcommands (behind `cli` feature) |
| WebSocket events | ✅ | Real-time `job.pushed/completed/failed/cancelled` at `/api/v1/events` |
| OpenTelemetry | ✅ | OTLP tracing export behind `otel` feature |

**Implementation:** 16 commits, 127+ tests (with sqlite feature). StorageBackend trait expanded to 16 async methods.

**Exit criteria:** ✅ All storage backends pass 14-test canonical harness. Library embeddable with zero config. Background scheduler enforces timeouts and promotes delayed jobs.

### Phase 3: Production Hardening & Dashboard (v0.3) — Weeks 13-18 ✅ COMPLETE

**Goal:** Production-ready observability, web dashboard, and operational hardening.

| Deliverable | Status | Description |
|---|---|---|
| Prometheus metrics | ✅ (Phase 1) | Jobs pushed/completed/failed/pulled counters |
| Structured logging | ✅ (Phase 1) | JSON logs with tracing context |
| Health check | ✅ (Phase 1) | `/health` endpoint |
| SQLite backend | ✅ (Phase 2) | Alternative storage option |
| OpenTelemetry | ✅ (Phase 2) | OTLP trace export |
| Bearer token auth (HTTP) | ✅ | Public/protected router split, `Authorization: Bearer <token>` |
| Bearer token auth (TCP) | ✅ | Connection-level `{"cmd":"auth","token":"..."}` handshake |
| CORS + Request tracing | ✅ | `tower-http` CorsLayer + TraceLayer middleware |
| Graceful shutdown | ✅ | `watch` channel, 30s drain timeout, Ctrl+C signal handling |
| Retention auto-cleanup | ✅ | TTL parsing ("7d"/"24h"/"30m"), scheduler-driven expired job removal |
| TLS for TCP | ✅ | `rustls` + `tokio-rustls` behind `tls` feature flag |
| Web dashboard | ✅ | Embedded SPA via `rust-embed`: overview, queues, DLQ, live events |
| Landing page | ✅ | Marketing page at `/` with feature showcase |
| Production smoke tests | ✅ | 8-step integration test covering auth, push, pull, ack, DLQ, metrics |

**Implementation:** 10 commits, 149+ tests (default), 157+ tests (with sqlite). StorageBackend trait expanded to 18 async methods.

**Exit criteria:** ✅ Dashboard shows all system state. Auth protects all endpoints. Graceful shutdown drains connections with 30s timeout.

### Phase 4: Schedule Engine & Production Completeness (v0.4) — Weeks 19-26 ✅ COMPLETE

**Goal:** Full schedule execution engine, schedule management APIs, dashboard integration, and production readiness polish.

| Deliverable | Status | Description |
|---|---|---|
| Storage trait expansion | ✅ | `get_schedule()` + `list_all_schedules()` — trait now 20 async methods |
| Schedule CRUD | ✅ | create/get/list/delete/pause/resume in QueueManager with validation |
| Schedule execution engine | ✅ | Background tick loop evaluates cron (via croner) + interval schedules |
| Schedule API (HTTP) | ✅ | 6 REST endpoints: POST/GET/DELETE /schedules, pause/resume |
| Schedule TCP commands | ✅ | 6 commands: schedule_create/list/get/delete/pause/resume |
| Schedule CLI commands | ✅ | `rustqueue schedules list/create/delete/pause/resume` |
| Dashboard schedules panel | ✅ | Schedule cards with status, timing, execution count, pause/resume toggle |
| Dashboard auth hardening | ✅ | Dashboard protected behind auth when authentication is enabled |
| PostgreSQL serve mode | ✅ | Replaced placeholder with actual PostgresStorage initialization |
| Integration tests | ✅ | 4 lifecycle tests: interval, cron, pause, max_executions |

**Phase 4 stats:** 10 tasks, 10 commits, 158+ tests (default at Phase 4), 184+ with sqlite (at Phase 4)

**Exit criteria:** ✅ Schedule engine executes cron/interval jobs, API covers full CRUD, dashboard shows schedules, all backends support schedule storage.

### Phase 5: Performance Optimization (v0.10) — Weeks 27-30+ ✅ COMPLETE

**Goal:** Close the single-job throughput gap while preserving the large gains from batched/coalesced paths. See `docs/performance-analysis.md` for root-cause analysis and roadmap.

| Deliverable | Status | Description |
|---|---|---|
| Batch transaction API | ✅ | `insert_jobs_batch()` implemented and used by API/engine batch push |
| spawn_blocking for redb | ✅ | redb sync I/O moved off async workers via blocking pool |
| Secondary index tables | ✅ | queue/state/priority scans replaced with indexed dequeue and query paths |
| Atomic single-job completion path | ✅ | `complete_job()` backend API reduces ack transition overhead |
| Write coalescing primitive | ✅ | `complete_jobs_batch()` implemented with redb single-transaction override |
| Batched TCP commands | ✅ | `push_batch` + `ack_batch` now supported in protocol and tests |
| Competitor benchmark suite extensions | ✅ | Redis, RabbitMQ, BullMQ, Celery + RustQueue batch controls + `--write-coalescing` flag |
| Automatic timed coalescing (single-job path) | ✅ | `BufferedRedbStorage` — server-side buffered writes with configurable flush interval |
| Unique key index | ✅ | `JOBS_UNIQUE_KEY_INDEX` — O(1) lookup for `get_job_by_unique_key()` |
| Index-based cleanup | ✅ | `remove_*_before()` uses `JOBS_STATE_UPDATED_INDEX` prefix scan — O(K) not O(N) |
| Queue names from index | ✅ | `list_queue_names()` extracts from index keys without JSON deserialization |
| Hybrid memory+disk storage | ✅ | DashMap in-memory hot path with periodic redb snapshots (`src/storage/hybrid.rs`) |
| DashMap MemoryStorage | ✅ | Lock-free concurrent access replacing `RwLock<HashMap>` |
| Per-queue table sharding | Won't do | redb single-writer model makes table sharding ineffective; index prefix partitioning + BufferedRedb/HybridStorage solve this better |

**Phase 5 stats:** 4 redb tables, all O(N) scans eliminated, hybrid storage added, DashMap MemoryStorage

**Benchmark results (redb, 2026-02-06, Criterion):**

| Profile | Raw RedbStorage | BufferedRedbStorage | Speedup |
|---|---|---|---|
| 10 concurrent pushes | 25.5ms (392/s) | 15.1ms (663/s) | 1.7x |
| 50 concurrent pushes | 174.2ms (287/s) | 15.8ms (3,165/s) | 11.0x |
| 100 concurrent pushes | 272.5ms (367/s) | **4.5ms (22,222/s)** | **60.6x** |

**References:** `docs/performance-analysis.md`, `docs/release-checklist-v0.10.md`

**Remaining (deferred):**

1. Re-run competitor suite with hybrid storage mode (`--hybrid` flag now wired up).

**Exit criteria:** ✅ ≥ 10,000 push/sec achieved (22,222/s at 100 concurrent with write coalescing). Hybrid storage available for in-memory speed with disk durability.

### Phase 6: Production Readiness & Ecosystem (v0.11) — Weeks 31-34 ✅ COMPLETE

**Goal:** Security hardening, comprehensive observability, client SDKs, and deployment tooling.

| Deliverable | Status | Description |
|---|---|---|
| Input validation | ✅ | Max queue name (256), job name (1024), data (1MB), unique key (1024), error message (10KB) |
| Queue pause/resume | ✅ | `POST /queues/{queue}/pause\|resume`, rejects pushes to paused queues with 503 |
| Auth rate limiting | ✅ | 5 failed attempts = 5-minute lockout per IP via DashMap tracker |
| Gauge metrics | ✅ | Per-queue waiting/active/delayed/dlq gauges, scheduler tick duration, WebSocket clients |
| Latency histograms | ✅ | `push_duration_seconds`, `pull_duration_seconds`, `ack_duration_seconds` |
| HTTP error rate middleware | ✅ | Request count + latency per response status code |
| Grafana dashboard | ✅ | Pre-built dashboard JSON at `docs/grafana/rustqueue-dashboard.json` |
| Node.js SDK | ✅ | TypeScript, HTTP + TCP clients, ESM/CJS, zero runtime deps (`sdk/node/`) |
| Python SDK | ✅ | stdlib-only HTTP client, Python >= 3.8 (`sdk/python/`) |
| Docker deployment | ✅ | Dockerfile, docker-compose.yml, docker-compose.monitoring.yml (Prometheus + Grafana) |
| Deploy configs | ✅ | Production TOML, Prometheus scrape config, Grafana provisioning (`deploy/`) |

**Phase 6 stats:** 212+ tests (default), ~228 with sqlite. 15+ Prometheus metrics.

**Exit criteria:** ✅ All security features active, metrics cover gauges + histograms, SDKs cover full API surface, Docker one-command deployment works.

### Phase 7: TCP & Consume Performance (v0.12) — Weeks 35-36 ✅ COMPLETE

**Goal:** Beat RabbitMQ on produce throughput and eliminate consume bottleneck in HybridStorage.

| Deliverable | Status | Description |
|---|---|---|
| TCP_NODELAY | ✅ | Disable Nagle's algorithm on accepted TCP connections |
| BufWriter + flush | ✅ | Application-level write buffering with explicit flush for fewer syscalls |
| TCP pipelining | ✅ | Read all buffered lines before processing, single flush per batch response |
| Clone reduction | ✅ | Eliminate unnecessary `cmd.clone()` in TCP push handlers |
| estimate_json_size | ✅ | Walk Value tree for fast payload size check without serialization |
| Stack-allocated index key | ✅ | `state_updated_key()` returns `[u8; 25]` instead of `Vec<u8>` |
| Eventual durability for hybrid | ✅ | Force `Eventual` redb durability for hybrid inner backend (safe since hybrid accepts data loss) |
| Per-queue BTreeSet waiting index | ✅ | HybridStorage dequeue uses BTreeSet — O(log N) instead of O(total_jobs) full-table scan |

**Phase 7 stats:** 221+ tests passing. RustQueue beats RabbitMQ across all three metrics.

**Benchmark results (February 7, 2026, hybrid TCP):**

| Metric | batch_size=1 | batch_size=50 | RabbitMQ (batch=50) |
|---|---|---|---|
| Produce | 23,382/s | **43,494/s** | 35,975/s |
| Consume | 10,987/s | **14,195/s** | 2,675/s |
| End-to-end | 9,486/s | **14,681/s** | 2,902/s |

**Key win — consume throughput:** Per-queue BTreeSet waiting index improved consume from 292/s to 10,987/s (**37.6x improvement**). The old O(N) DashMap full scan was the bottleneck.

**Exit criteria:** ✅ RustQueue beats RabbitMQ on produce (1.2x), consume (5.3x), and end-to-end (5.1x). Consume bottleneck eliminated.

### Phase 8: Webhooks & DAG Flows (v0.13) — Weeks 37-38 ✅ COMPLETE

**Goal:** Event-driven integrations via webhooks and job dependency graphs for multi-step workflows.

| Deliverable | Status | Description |
|---|---|---|
| WebhookManager | ✅ | In-memory DashMap storage, HMAC-SHA256 signing, retry delivery with exponential backoff |
| Webhook HTTP API | ✅ | `POST/GET/DELETE /api/v1/webhooks` — register, list, get, delete |
| Webhook TCP commands | ✅ | `webhook_create`, `webhook_list`, `webhook_delete` |
| Webhook event dispatch | ✅ | Background task subscribes to broadcast channel, matches events to webhooks, delivers with retries |
| Webhook events | ✅ | JobPushed, JobCompleted, JobFailed, JobDlq, JobCancelled, JobProgress |
| DAG dependency resolution | ✅ | `depends_on: Vec<JobId>` on Job, inline promotion on `ack()`, Blocked→Waiting |
| BFS cycle detection | ✅ | Detects cycles at push time, configurable `max_dag_depth` (default: 10) |
| Cascade DLQ failure | ✅ | Parent DLQ → recursive cascade of Blocked children to DLQ via `Box::pin()` |
| Flow status endpoint | ✅ | `GET /api/v1/flows/{flow_id}` — all jobs + summary counts |
| Scheduler safety net | ✅ | Every 10 ticks, promote orphaned Blocked jobs whose deps are all Completed |
| `get_jobs_by_flow_id` | ✅ | Added to StorageBackend trait (24 methods), implemented in all backends |
| Go SDK | ✅ | HTTP + TCP clients, 23 tests, 2 examples, `AckBatch` support (`sdk/go/`) |
| OpenAPI spec | ✅ | OpenAPI 3.1 with 21 endpoints, 36 schemas, Scalar UI at `/api/v1/docs` |

**Phase 8 stats:** 249+ tests passing. StorageBackend trait expanded to 24 async methods. 3 client SDKs (Node.js, Python, Go).

**Exit criteria:** ✅ Webhooks fire on job lifecycle events with HMAC signing. DAG flows resolve dependencies inline on ack. Cascade failure propagates to all blocked children. Go SDK and OpenAPI spec complete.

### Phase 9: Distributed Mode (v0.5) — Weeks 39-48

**Goal:** High-availability cluster mode.

| Deliverable | Status | Description |
|---|---|---|
| Raft consensus | ⬜ | Leader election, log replication using `openraft` |
| Cluster discovery | ⬜ | Static peers and DNS-based discovery |
| Follower reads | ⬜ | Read operations served by followers |
| Failover | ⬜ | Automatic leader re-election on failure |
| Cluster dashboard | ⬜ | Node status, Raft log visualization |
| Postgres backend | ✅ (Phase 2) | External storage for shared-nothing deployments |

**Exit criteria:** 3-node cluster survives leader failure with < 10s failover and zero job loss.

### v1.0 — Week 37+

Stabilize APIs, write migration guides, achieve production use at 3+ organizations.

---

## 18. Success Metrics

### 18.1 Adoption Metrics

| Metric | 6-Month Target | 12-Month Target |
|---|---|---|
| GitHub stars | 1,000 | 5,000 |
| Docker pulls | 5,000 | 50,000 |
| Crates.io downloads | 2,000 | 20,000 |
| npm SDK downloads | 1,000 | 10,000 |
| Active production deployments (estimated) | 50 | 500 |
| Contributors | 10 | 30 |

### 18.2 Technical Metrics

| Metric | Target | Current (v0.13) | Status |
|---|---|---|---|
| Throughput (push, hybrid TCP batch_size=50) | ≥ 50,000 jobs/sec | **~43,494/sec** | Within 1.15x |
| Throughput (push, hybrid TCP batch_size=1) | ≥ 30,000 jobs/sec | **~23,382/sec** | Within 1.3x |
| Throughput (consume, hybrid TCP batch_size=50) | ≥ 10,000 jobs/sec | **~14,195/sec** | PASS |
| Throughput (end-to-end, hybrid TCP batch_size=50) | ≥ 10,000 jobs/sec | **~14,681/sec** | PASS |
| Throughput (push, 100 concurrent + write coalescing) | ≥ 50,000 jobs/sec | **~22,222/sec** | Within 2.3x |
| Beats RabbitMQ on produce | > 30,000 jobs/sec | **43,494/sec** (1.2x RabbitMQ) | PASS |
| Beats RabbitMQ on consume | > 3,000 jobs/sec | **14,195/sec** (5.3x RabbitMQ) | PASS |
| Unique key lookup | O(1) | O(1) index | PASS |
| Cleanup (retention) | O(K) | O(K) indexed | PASS |
| P99 latency (push) | < 5 ms | ~3 ms | OK |
| Prometheus metrics | 15+ | 15+ (counters + gauges + histograms) | PASS |
| Binary size | < 15 MB | 6.8 MB | PASS |
| Memory usage (idle) | < 20 MB | ~15 MB | PASS |
| Startup time | < 500 ms | ~10 ms | PASS |
| Test coverage | > 80% | 249+ tests | OK |
| Client SDKs | 3 | Node.js + Python + Go | PASS |
| Input validation | Complete | All fields validated | PASS |
| Zero CVEs | Continuous | 0 | PASS |

### 18.3 Community Metrics

| Metric | Target |
|---|---|
| Hacker News front page | At least 1 launch post |
| Conference talks | 2+ (RustConf, local meetups) |
| Blog posts / tutorials by others | 10+ |
| Discord / community members | 500+ |

---

## 19. Business Model

### 19.1 Open Source Core (MIT License)

The core RustQueue binary is fully open-source and MIT-licensed. This includes:
- All job queue functionality
- Single-node deployment
- Built-in dashboard
- All storage backends
- All APIs and protocols
- All SDKs

### 19.2 Revenue Streams (Future)

#### Option A: Open Core + Commercial License

| Tier | Price | Features |
|---|---|---|
| **Community** | Free | Everything in the open-source release |
| **Pro** | $49/month per node | Cluster mode (Raft), OIDC/SSO, audit logs, priority support |
| **Enterprise** | Custom | Multi-tenancy, custom integrations, SLA, dedicated support |

#### Option B: Managed Cloud Service

| Tier | Price | Features |
|---|---|---|
| **Starter** | Free | 10,000 jobs/month, 1 queue, community support |
| **Growth** | $29/month | 500,000 jobs/month, unlimited queues, email support |
| **Scale** | $99/month | 5,000,000 jobs/month, HA cluster, priority support |
| **Enterprise** | Custom | Unlimited, dedicated infrastructure, SLA |

#### Option C: Dual License (AGPL + Commercial)

- Open source under AGPL (requires open-sourcing derivative works)
- Commercial license for proprietary use ($X/year)
- Used successfully by GitLab, Minio, and others

### 19.3 Recommended Strategy

Start with **MIT license** for maximum adoption. Once traction is established (1,000+ GitHub stars, 50+ production users), introduce a **managed cloud service** (Option B) as the primary revenue stream. This avoids the community friction of open-core feature gating while building a sustainable business.

---

## 20. Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| **Rust learning curve slows development** | Medium | High | Leverage well-established crates (axum, tokio, serde). Start with the simplest features first. |
| **Storage performance issues at scale** | Low | High | Benchmark early and often. The trait-based storage layer allows swapping backends. |
| **Raft implementation complexity** | High | Medium | Use the battle-tested `openraft` crate. Defer cluster mode to v0.5 after core is stable. |
| **Low adoption / crowded market** | Medium | High | Differentiate on "zero deps, single binary." Target the underserved solo dev and small team segments first. |
| **BullMQ/Sidekiq add Rust competition** | Low | Medium | Move fast on the features they can't easily add (embedded mode, WASM, single binary). |
| **Scope creep toward workflow orchestration** | Medium | Medium | Stay firm on "not Temporal." RustQueue is a job queue with scheduling. Refer workflow users to Temporal. |
| **Dashboard becomes a maintenance burden** | Medium | Low | Keep the dashboard minimal and data-driven. All data comes from the same API users have access to. |

---

## 21. Future Roadmap (Post v1.0)

Items explicitly out of scope for v1.0 but planned for the future:

| Feature | Description | Timeframe |
|---|---|---|
| **WASM SDK** | Core scheduling logic compiled to WASM for browser-side use | v1.1 |
| **Job batching** | Process a batch of jobs as a single unit (all-or-nothing) | v1.2 |
| **Multi-tenancy** | Namespace isolation for different teams/services | v1.2 |
| **Audit log** | Immutable record of all administrative actions | v1.2 |
| **Geographic routing** | Route jobs to workers in specific regions | v1.3 |
| **Plugin system** | User-defined middleware for job lifecycle hooks | v2.0 |
| **GUI job builder** | Visual DAG editor for building flows in the dashboard | v2.0 |
| **S3/GCS result storage** | Store large job results in object storage | v2.0 |

---

## 22. Technical Stack

| Component | Technology | Rationale |
|---|---|---|
| **Language** | Rust (2024 edition) | Memory safety, performance, single binary |
| **Async runtime** | tokio | Industry standard, mature, well-documented |
| **HTTP framework** | axum | Best ergonomics, tower middleware, WebSocket support |
| **TCP protocol** | tokio raw TCP + serde_json | Maximum performance, minimal overhead |
| **Storage (default)** | redb | Pure Rust, embedded, ACID, zero external deps |
| **Storage (optional)** | rusqlite | SQLite familiarity, great debugging tools |
| **Storage (optional)** | sqlx + PostgreSQL | For teams with existing Postgres |
| **Cron parsing** | croner | Full cron syntax + L/W/# extensions |
| **Consensus** | openraft | Battle-tested Raft implementation |
| **Metrics** | metrics + metrics-exporter-prometheus | Prometheus-compatible |
| **Logging** | tracing + tracing-subscriber | Structured, async-aware logging |
| **CLI** | clap | Standard Rust CLI framework |
| **Config** | config-rs | Multi-source (file, env, CLI) configuration |
| **Serialization** | serde + serde_json | Universal Rust serialization |
| **TLS** | rustls | No OpenSSL dependency |
| **Dashboard embed** | rust-embed | Compile static assets into binary |
| **Dashboard UI** | Preact + Tailwind CSS | Minimal bundle size (< 500 KB) |
| **IDs** | uuid v7 | Time-sortable, globally unique |
| **Testing** | cargo test + proptest | Unit, integration, and property-based tests |
| **CI/CD** | GitHub Actions | Build, test, release binaries, publish Docker images |
| **Benchmarks** | criterion | Reproducible micro-benchmarks |

---

## 23. Glossary

| Term | Definition |
|---|---|
| **Job** | A unit of work submitted to a queue for processing by a worker |
| **Queue** | A named, ordered collection of jobs |
| **Worker** | A process that pulls jobs from a queue and executes them |
| **Schedule** | A recurring or one-time trigger that creates jobs at specified times |
| **Flow** | A directed acyclic graph (DAG) of jobs with dependency relationships |
| **DLQ** | Dead Letter Queue — where jobs go after exhausting all retry attempts |
| **Backoff** | The strategy for increasing delay between retry attempts |
| **Stall** | When a worker stops sending heartbeats while processing a job |
| **Raft** | A consensus algorithm for replicating state across distributed nodes |
| **WAL** | Write-Ahead Log — a technique for ensuring durability before acknowledging writes |
| **redb** | A pure-Rust embedded key-value database with ACID transactions |
| **SSE** | Server-Sent Events — a unidirectional real-time protocol over HTTP |

---

*End of document.*
