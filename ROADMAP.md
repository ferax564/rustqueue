# RustQueue Roadmap

## Current State

Phases 1-5 are complete. Schedule engine, dashboard, DAG workflows, webhooks, and multi-backend support are stable. Distributed mode remains the major open milestone.

## Priorities

### P0 — Distributed Mode

- Raft leader election and log replication
- Automatic failover and node membership
- Cluster health visibility and observability

### P1 — Performance & Operations

- Per-queue sharding for horizontal scaling
- Batch-default client behavior for higher throughput
- Multi-node cluster observability improvements

### P2 — Platform Features

- Multi-tenant controls and audit logging
- GUI flow builder for DAG workflows

## Success Criteria

- 3-node cluster survives leader failure with no job loss and bounded failover time
- Throughput improves for both single-job and batched workloads
