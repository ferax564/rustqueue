# RustQueue Roadmap

Last updated: 2026-02-07

## Current State

- Phases 1-5 are complete.
- Schedule engine, dashboard, and multi-backend support are in production.
- Throughput gains from buffered writes are validated; distributed mode remains the major open milestone.

## Organization Role

- RustQueue is the single orchestration spine for ForgePipe and all engine workflows.
- Scheduling, retries, DLQ, workflow state, and distributed coordination are RustQueue-owned capabilities.

## Next Priorities

### P0 (Now)

1. Deliver Phase 6 distributed mode alpha:
   - Raft leader election and replication
   - failover behavior and node membership primitives
   - cluster health visibility
2. Add workflow metadata and execution primitives for ForgePipe DAG orchestration:
   - workflow id / step id / artifact refs
   - dependency tracking and step-state transitions
3. Stabilize orchestration APIs consumed by ForgePipe workers and templates.

### Phase A Task Mapping (Current)

1. `RQ-A1`: workflow metadata compatibility and lifecycle persistence.
2. `RQ-A2`: orchestration API stabilization for ForgePipe coordinator.

Downstream tasks unblocked by this work:

1. `FP-A4` workflow coordinator execution.
2. `OX-A1`, `PF-A1`, and `FR-A1` adapter wiring in engine repos.

### P1 (Next)

1. Phase 5b performance backlog:
   - hybrid memory+disk mode
   - per-queue sharding design/implementation
2. Batch-default client behavior for higher throughput under real workloads.
3. Operations improvements for multi-node cluster observability.

### P2 (Later)

1. Multi-tenant controls and audit log tracks.
2. GUI flow builder after core distributed reliability is stable.

## Success Gates

- 3-node cluster survives leader failure with no job loss and bounded failover.
- ForgePipe workflow engine runs on RustQueue primitives without additional scheduler services.
- Throughput trend improves for both single-job and batched profiles.
