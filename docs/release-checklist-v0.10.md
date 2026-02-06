# RustQueue v0.10 Release Readiness Checklist

**Date:** February 6, 2026
**Version:** v0.10 (unique key index + index-based cleanup + queue names from index + BufferedRedbStorage)

---

## Code Quality

- [x] All tests pass (`cargo test` — 183 tests, 0 failures)
- [x] All tests pass with sqlite feature (`cargo test --features sqlite` — ~199 tests)
- [x] `cargo clippy` — 3 pre-existing warnings only (large_enum_variant on `CompleteJobOutcome`, not introduced by v0.10)
- [x] `cargo check --benches` — benchmarks compile
- [x] No new unsafe code introduced
- [x] No new dependencies added

## New Features

### Unique Key Index (`JOBS_UNIQUE_KEY_INDEX`)
- [x] New redb table `jobs_unique_key_idx` created on startup
- [x] `insert_job_indexes` / `remove_job_indexes` updated to accept 3 table handles
- [x] All 12+ callers updated to open and pass the new index table
- [x] Only non-terminal states indexed (not Completed/Dlq/Cancelled)
- [x] `get_job_by_unique_key` rewritten as O(1) index lookup
- [x] `rebuild_indexes_if_needed` clears and rebuilds the new index
- [x] Backend tests pass (tests unique key lookups)

### Index-Based Cleanup
- [x] `remove_completed_before` scans `JOBS_STATE_UPDATED_INDEX` (Completed prefix)
- [x] `remove_failed_before` scans `JOBS_STATE_UPDATED_INDEX` (Failed prefix) with early-break
- [x] `remove_dlq_before` scans `JOBS_STATE_UPDATED_INDEX` (Dlq prefix) with early-break
- [x] `remove_completed_before` checks `completed_at` from job (not `updated_at` from index key)
- [x] Backend tests pass (test_remove_completed_before, test_remove_failed_before, etc.)

### Queue Names from Index
- [x] `list_queue_names` extracts queue name bytes from `JOBS_QUEUE_STATE_PRIORITY_INDEX` keys
- [x] No JSON deserialization needed
- [x] Deduplication via `BTreeSet`

### BufferedRedbStorage (Write Coalescing)
- [x] New file `src/storage/buffered_redb.rs`
- [x] Implements full `StorageBackend` trait (all 20+ methods)
- [x] Single `insert_job`/`complete_job` buffered with oneshot channels
- [x] Background flush task with `tokio::select!` on timer (10ms) vs notify (batch full)
- [x] `dequeue` calls `flush_inserts()` first for visibility guarantee
- [x] Batch operations (`insert_jobs_batch`, `complete_jobs_batch`) bypass buffer
- [x] All reads pass through to inner storage
- [x] All 18 `backend_tests!` macro tests pass against `BufferedRedbStorage`
- [x] Config: `write_coalescing_enabled`, `write_coalescing_interval_ms`, `write_coalescing_max_batch`
- [x] Wired in `src/main.rs` (wrap RedbStorage when enabled)
- [x] Wired in `src/builder.rs` (`with_write_coalescing` method)

## Configuration

- [x] New `[storage]` config fields have sensible defaults (disabled by default)
- [x] `write_coalescing_enabled = false` — opt-in, no behavior change for existing users
- [x] `write_coalescing_interval_ms = 10` — reasonable default flush interval
- [x] `write_coalescing_max_batch = 100` — reasonable default batch size

## Backward Compatibility

- [x] Write coalescing is disabled by default — zero behavior change for existing deployments
- [x] Existing redb databases upgraded transparently (new index table created on open)
- [x] `rebuild_indexes_if_needed` handles migration from pre-v0.10 databases
- [x] No changes to public API (StorageBackend trait, QueueManager, HTTP/TCP protocol)
- [x] No changes to wire format (JSON protocol unchanged)

## Documentation

- [x] `CLAUDE.md` updated with v0.10 architecture and performance info
- [x] `docs/performance-analysis.md` updated with v0.10 implementation details
- [x] Optimization plan table updated (items 8-11 marked Done)
- [x] Comparison to targets table updated with v0.10 estimates

## Benchmarks

- [x] Criterion benchmarks updated with 3 new `BufferedRedbStorage` variants
- [x] `scripts/benchmark_competitors.py` supports `--write-coalescing` flag
- [x] Write coalescing setting recorded in JSON and markdown reports
- [x] `cargo bench` run — concurrent_push_buffered/100: 4.5ms/100 jobs (**22,222 jobs/sec**, 60.6x vs raw)
- [ ] **TODO:** Run competitor suite with `--write-coalescing` flag (requires Docker + Python packages)
- [ ] **TODO:** Run competitor suite without coalescing for control baseline (requires Docker + Python packages)

## Pre-Release Actions

- [ ] Bump `Cargo.toml` version from `0.1.0` to target version
- [x] Run `cargo bench` — results: raw 272.5ms/100 concurrent, buffered 4.5ms/100 concurrent (60.6x speedup)
- [ ] Run competitor benchmarks: `python scripts/benchmark_competitors.py --ops 1000 --repeats 3 --write-coalescing`
- [ ] Run competitor benchmarks (control): `python scripts/benchmark_competitors.py --ops 1000 --repeats 3`
- [x] Update `docs/performance-analysis.md` with actual benchmark numbers
- [ ] Run `cargo build --release` and verify binary size
- [ ] Tag release

## Risk Assessment

| Area | Risk | Mitigation |
|------|------|------------|
| Write coalescing durability | Up to 10ms of data loss on crash | Disabled by default; documented trade-off |
| Unique key index size | Additional storage for index entries | Only non-terminal states; cleaned up on state transition |
| Index rebuild on upgrade | First startup after upgrade may be slower | One-time cost; rebuild_indexes_if_needed handles it |
| BufferedRedbStorage memory | Pending operations held in memory | Bounded by max_batch (default 100); flushed on interval |
