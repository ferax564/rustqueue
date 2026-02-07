//! Hybrid memory+disk storage backend for maximum throughput.
//!
//! All reads and writes hit an in-memory `DashMap` for near-zero latency.
//! A background task periodically snapshots dirty entries to an inner
//! [`StorageBackend`] (typically redb) for durability.
//!
//! **Trade-off:** up to `snapshot_interval` of data can be lost on crash.
//! For many workloads this is acceptable since workers will retry.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::DashSet;
use tracing::{debug, warn};

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::{CompleteJobOutcome, StorageBackend};

/// Configuration for the hybrid storage layer.
#[derive(Debug, Clone)]
pub struct HybridConfig {
    /// How often to snapshot dirty entries to disk (milliseconds).
    pub snapshot_interval_ms: u64,
    /// Maximum dirty entries before triggering an early flush.
    pub max_dirty_before_flush: usize,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            snapshot_interval_ms: 1000,
            max_dirty_before_flush: 5000,
        }
    }
}

/// Tracks what kind of mutation happened to a job.
#[derive(Debug, Clone, Copy)]
enum DirtyKind {
    /// Job was inserted or updated — persist via upsert.
    Upsert,
    /// Job was deleted — remove from disk.
    Delete,
}

/// Index key for the per-queue waiting set: (priority DESC, created_at ASC, id).
type WaitingKey = (Reverse<i32>, DateTime<Utc>, JobId);

fn waiting_key(job: &Job) -> WaitingKey {
    (Reverse(job.priority), job.created_at, job.id)
}

/// Shared in-memory state accessed by both the main storage and the flush loop.
struct SharedState {
    jobs: DashMap<JobId, Job>,
    schedules: RwLock<HashMap<String, Schedule>>,
    dirty_jobs: DashMap<JobId, DirtyKind>,
    dirty_schedules: DashSet<String>,
    /// Per-queue index of `Waiting` jobs, ordered by priority DESC then created_at ASC.
    /// Eliminates O(N) full-table scan in `dequeue()`.
    queue_waiting: DashMap<String, BTreeSet<WaitingKey>>,
}

/// Hybrid memory+disk storage that serves all operations from memory
/// and asynchronously persists to a durable backend.
pub struct HybridStorage {
    state: Arc<SharedState>,
    inner: Arc<dyn StorageBackend>,
    notify: Arc<tokio::sync::Notify>,
    _flush_handle: tokio::task::JoinHandle<()>,
    loaded: AtomicBool,
    max_dirty_before_flush: usize,
}

impl HybridStorage {
    /// Create a new hybrid storage wrapping the given durable backend.
    ///
    /// Starts a background snapshot task immediately. The in-memory state
    /// is populated from disk on the first operation.
    pub fn new(inner: Arc<dyn StorageBackend>, config: HybridConfig) -> Self {
        let state = Arc::new(SharedState {
            jobs: DashMap::new(),
            schedules: RwLock::new(HashMap::new()),
            dirty_jobs: DashMap::new(),
            dirty_schedules: DashSet::new(),
            queue_waiting: DashMap::new(),
        });
        let notify = Arc::new(tokio::sync::Notify::new());

        let flush_handle = tokio::spawn(Self::flush_loop(
            Arc::clone(&inner),
            Arc::clone(&state),
            Arc::clone(&notify),
            config.snapshot_interval_ms,
        ));

        Self {
            state,
            inner,
            notify,
            _flush_handle: flush_handle,
            loaded: AtomicBool::new(false),
            max_dirty_before_flush: config.max_dirty_before_flush,
        }
    }

    /// Load all existing data from the durable backend into memory.
    ///
    /// Called automatically on first operation. Safe to call multiple times.
    pub async fn load_from_disk(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }

        // Load all schedules
        let schedules = self.inner.list_all_schedules().await?;
        {
            let mut map = self.state.schedules.write().unwrap();
            for schedule in schedules {
                map.insert(schedule.name.clone(), schedule);
            }
        }

        // Load active jobs
        let active_jobs = self.inner.get_active_jobs().await?;
        for job in active_jobs {
            self.state.jobs.insert(job.id, job);
        }

        // Load DLQ jobs for each queue
        let queue_names = self.inner.list_queue_names().await?;
        for queue in &queue_names {
            let dlq_jobs = self.inner.get_dlq_jobs(queue, u32::MAX).await?;
            for job in dlq_jobs {
                self.state.jobs.insert(job.id, job);
            }
        }

        // Load delayed jobs (use far-future time to get all)
        let far_future = Utc::now() + chrono::Duration::days(365 * 100);
        let delayed_jobs = self.inner.get_ready_scheduled(far_future).await?;
        for job in delayed_jobs {
            self.state.jobs.insert(job.id, job);
        }

        // NOTE: Waiting/Completed/Failed jobs cannot be loaded through the current
        // StorageBackend trait (no "list all" method). Jobs created through this
        // hybrid layer will be in memory. For existing data, consider migrating
        // with a full table scan at startup.

        // Populate the per-queue waiting index from loaded jobs.
        for entry in self.state.jobs.iter() {
            let job = entry.value();
            if job.state == JobState::Waiting {
                self.state
                    .queue_waiting
                    .entry(job.queue.clone())
                    .or_default()
                    .insert(waiting_key(job));
            }
        }

        self.loaded.store(true, Ordering::Release);
        debug!(
            jobs_loaded = self.state.jobs.len(),
            "Hybrid storage loaded from disk"
        );
        Ok(())
    }

    async fn ensure_loaded(&self) -> Result<()> {
        if !self.loaded.load(Ordering::Acquire) {
            self.load_from_disk().await?;
        }
        Ok(())
    }

    /// Add a job to the per-queue waiting index (if state is Waiting).
    fn index_add(&self, job: &Job) {
        if job.state == JobState::Waiting {
            self.state
                .queue_waiting
                .entry(job.queue.clone())
                .or_default()
                .insert(waiting_key(job));
        }
    }

    /// Remove a job from the per-queue waiting index.
    fn index_remove(&self, job: &Job) {
        if let Some(mut entry) = self.state.queue_waiting.get_mut(&job.queue) {
            entry.value_mut().remove(&waiting_key(job));
        }
    }

    fn mark_dirty(&self, id: JobId, kind: DirtyKind) {
        self.state.dirty_jobs.insert(id, kind);
        if self.state.dirty_jobs.len() >= self.max_dirty_before_flush {
            self.notify.notify_one();
        }
    }

    fn mark_schedule_dirty(&self, name: &str) {
        self.state.dirty_schedules.insert(name.to_string());
    }

    async fn flush_loop(
        inner: Arc<dyn StorageBackend>,
        state: Arc<SharedState>,
        notify: Arc<tokio::sync::Notify>,
        interval_ms: u64,
    ) {
        let interval = tokio::time::Duration::from_millis(interval_ms);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = notify.notified() => {}
            }
            Self::flush_once(&inner, &state).await;
        }
    }

    async fn flush_once(inner: &Arc<dyn StorageBackend>, state: &SharedState) {
        // Drain dirty jobs atomically
        let dirty: Vec<(JobId, DirtyKind)> = {
            let entries: Vec<_> = state
                .dirty_jobs
                .iter()
                .map(|entry| (*entry.key(), *entry.value()))
                .collect();
            for (id, _) in &entries {
                state.dirty_jobs.remove(id);
            }
            entries
        };

        if !dirty.is_empty() {
            let mut upsert_count = 0usize;
            let mut delete_count = 0usize;

            for (id, kind) in &dirty {
                match kind {
                    DirtyKind::Upsert => {
                        if let Some(entry) = state.jobs.get(id) {
                            let job = entry.value();
                            // redb's insert_job is effectively an upsert (overwrites existing)
                            if let Err(e) = inner.insert_job(job).await {
                                warn!(job_id = %id, error = %e, "Failed to persist job");
                            } else {
                                upsert_count += 1;
                            }
                        }
                    }
                    DirtyKind::Delete => {
                        if let Err(e) = inner.delete_job(*id).await {
                            warn!(job_id = %id, error = %e, "Failed to delete job from disk");
                        } else {
                            delete_count += 1;
                        }
                    }
                }
            }

            if upsert_count > 0 || delete_count > 0 {
                debug!(upserts = upsert_count, deletes = delete_count, "Flushed jobs to disk");
            }
        }

        // Drain dirty schedules
        let dirty_names: Vec<String> = {
            let names: Vec<String> = state
                .dirty_schedules
                .iter()
                .map(|e| e.clone())
                .collect();
            for name in &names {
                state.dirty_schedules.remove(name);
            }
            names
        };

        if !dirty_names.is_empty() {
            // Clone schedule data out of the lock before async operations
            let schedule_snapshots: Vec<(String, Option<Schedule>)> = {
                let map = state.schedules.read().unwrap();
                dirty_names
                    .iter()
                    .map(|name| (name.clone(), map.get(name).cloned()))
                    .collect()
            };

            for (name, schedule_opt) in &schedule_snapshots {
                if let Some(schedule) = schedule_opt {
                    if let Err(e) = inner.upsert_schedule(schedule).await {
                        warn!(name, error = %e, "Failed to persist schedule");
                    }
                } else if let Err(e) = inner.delete_schedule(name).await {
                    warn!(name, error = %e, "Failed to delete schedule from disk");
                }
            }
            debug!(count = dirty_names.len(), "Flushed schedules to disk");
        }
    }
}

impl Drop for HybridStorage {
    fn drop(&mut self) {
        self._flush_handle.abort();
    }
}

#[async_trait]
impl StorageBackend for HybridStorage {
    // ── Job CRUD ────────────────────────────────────────────────────────

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        self.ensure_loaded().await?;
        let id = job.id;
        self.state.jobs.insert(id, job.clone());
        self.index_add(job);
        self.mark_dirty(id, DirtyKind::Upsert);
        Ok(id)
    }

    async fn insert_jobs_batch(&self, jobs: &[Job]) -> Result<Vec<JobId>> {
        self.ensure_loaded().await?;
        let mut ids = Vec::with_capacity(jobs.len());
        for job in jobs {
            self.state.jobs.insert(job.id, job.clone());
            self.index_add(job);
            self.mark_dirty(job.id, DirtyKind::Upsert);
            ids.push(job.id);
        }
        Ok(ids)
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        self.ensure_loaded().await?;
        Ok(self.state.jobs.get(&id).map(|r| r.value().clone()))
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        self.ensure_loaded().await?;

        // Snapshot old state before overwriting to maintain index consistency.
        let old_waiting_info: Option<(i32, DateTime<Utc>, String)> =
            self.state.jobs.get(&job.id).and_then(|entry| {
                let old = entry.value();
                if old.state == JobState::Waiting {
                    Some((old.priority, old.created_at, old.queue.clone()))
                } else {
                    None
                }
            });

        self.state.jobs.insert(job.id, job.clone());

        let was_waiting = old_waiting_info.is_some();
        let is_waiting = job.state == JobState::Waiting;

        if was_waiting && !is_waiting {
            // Waiting → other: remove from index
            let (pri, created, queue) = old_waiting_info.unwrap();
            if let Some(mut entry) = self.state.queue_waiting.get_mut(&queue) {
                entry.value_mut().remove(&(Reverse(pri), created, job.id));
            }
        } else if !was_waiting && is_waiting {
            // Other → Waiting: add to index (e.g., retry, delayed→waiting promotion)
            self.index_add(job);
        } else if was_waiting && is_waiting {
            // Still Waiting but priority or queue may have changed
            let (pri, created, queue) = old_waiting_info.unwrap();
            if let Some(mut entry) = self.state.queue_waiting.get_mut(&queue) {
                entry.value_mut().remove(&(Reverse(pri), created, job.id));
            }
            self.index_add(job);
        }

        self.mark_dirty(job.id, DirtyKind::Upsert);
        Ok(())
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        self.ensure_loaded().await?;
        if let Some((_, job)) = self.state.jobs.remove(&id) {
            if job.state == JobState::Waiting {
                self.index_remove(&job);
            }
        }
        self.mark_dirty(id, DirtyKind::Delete);
        Ok(())
    }

    async fn complete_job(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<CompleteJobOutcome> {
        self.ensure_loaded().await?;

        // Inline the completion logic so we avoid extra lookups
        let job = match self.state.jobs.get(&id) {
            Some(entry) => entry.value().clone(),
            None => return Ok(CompleteJobOutcome::NotFound),
        };

        if job.state != JobState::Active {
            return Ok(CompleteJobOutcome::InvalidState(job.state));
        }

        let now = Utc::now();
        let mut completed = job;
        completed.state = JobState::Completed;
        completed.completed_at = Some(now);
        completed.updated_at = now;
        completed.result = result;

        if completed.remove_on_complete {
            self.state.jobs.remove(&id);
            self.mark_dirty(id, DirtyKind::Delete);
        } else {
            self.state.jobs.insert(id, completed.clone());
            self.mark_dirty(id, DirtyKind::Upsert);
        }

        Ok(CompleteJobOutcome::Completed(completed))
    }

    async fn complete_jobs_batch(
        &self,
        items: &[(JobId, Option<serde_json::Value>)],
    ) -> Result<Vec<CompleteJobOutcome>> {
        let mut outcomes = Vec::with_capacity(items.len());
        for (id, result) in items {
            outcomes.push(self.complete_job(*id, result.clone()).await?);
        }
        Ok(outcomes)
    }

    // ── Queue operations ────────────────────────────────────────────────

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        self.ensure_loaded().await?;

        // Pop candidates from the per-queue waiting index — O(count * log N)
        // instead of the previous O(total_jobs) full-table scan.
        let candidate_ids: Vec<JobId> = {
            if let Some(mut entry) = self.state.queue_waiting.get_mut(queue) {
                let set = entry.value_mut();
                let mut ids = Vec::with_capacity(count as usize);
                while ids.len() < count as usize {
                    if let Some((_, _, id)) = set.pop_first() {
                        ids.push(id);
                    } else {
                        break;
                    }
                }
                ids
            } else {
                Vec::new()
            }
        };

        let now = Utc::now();
        let mut selected = Vec::new();

        for id in candidate_ids {
            if let Some(mut entry) = self.state.jobs.get_mut(&id) {
                let job = entry.value_mut();
                // Double-check state (concurrent dequeue race)
                if job.state == JobState::Waiting {
                    job.state = JobState::Active;
                    job.started_at = Some(now);
                    job.updated_at = now;
                    selected.push(job.clone());
                    self.mark_dirty(id, DirtyKind::Upsert);
                }
            }
        }

        Ok(selected)
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        self.ensure_loaded().await?;
        let mut counts = QueueCounts::default();
        for entry in self.state.jobs.iter() {
            let job = entry.value();
            if job.queue != queue {
                continue;
            }
            match job.state {
                JobState::Waiting | JobState::Created => counts.waiting += 1,
                JobState::Active => counts.active += 1,
                JobState::Delayed => counts.delayed += 1,
                JobState::Completed => counts.completed += 1,
                JobState::Failed => counts.failed += 1,
                JobState::Dlq => counts.dlq += 1,
                _ => {}
            }
        }
        Ok(counts)
    }

    // ── Scheduled jobs ──────────────────────────────────────────────────

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        self.ensure_loaded().await?;
        let ready = self
            .state
            .jobs
            .iter()
            .filter(|entry| {
                let j = entry.value();
                j.state == JobState::Delayed
                    && j.delay_until
                        .map(|delay_until| delay_until <= now)
                        .unwrap_or(false)
            })
            .map(|entry| entry.value().clone())
            .collect();
        Ok(ready)
    }

    // ── DLQ ─────────────────────────────────────────────────────────────

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        self.ensure_loaded().await?;
        if job.state == JobState::Waiting {
            self.index_remove(job);
        }
        let mut updated = job.clone();
        updated.state = JobState::Dlq;
        updated.last_error = Some(reason.to_string());
        updated.updated_at = Utc::now();
        self.state.jobs.insert(updated.id, updated);
        self.mark_dirty(job.id, DirtyKind::Upsert);
        Ok(())
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        self.ensure_loaded().await?;
        let dlq_jobs: Vec<Job> = self
            .state
            .jobs
            .iter()
            .filter(|entry| {
                let j = entry.value();
                j.queue == queue && j.state == JobState::Dlq
            })
            .take(limit as usize)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(dlq_jobs)
    }

    // ── Cleanup ─────────────────────────────────────────────────────────

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.ensure_loaded().await?;
        let to_remove: Vec<JobId> = self
            .state
            .jobs
            .iter()
            .filter(|entry| {
                let j = entry.value();
                j.state == JobState::Completed
                    && j.completed_at
                        .map(|completed_at| completed_at < before)
                        .unwrap_or(false)
            })
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len() as u64;
        for id in to_remove {
            self.state.jobs.remove(&id);
            self.mark_dirty(id, DirtyKind::Delete);
        }
        Ok(count)
    }

    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.ensure_loaded().await?;
        let to_remove: Vec<JobId> = self
            .state
            .jobs
            .iter()
            .filter(|entry| {
                let j = entry.value();
                j.state == JobState::Failed && j.updated_at < before
            })
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len() as u64;
        for id in to_remove {
            self.state.jobs.remove(&id);
            self.mark_dirty(id, DirtyKind::Delete);
        }
        Ok(count)
    }

    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.ensure_loaded().await?;
        let to_remove: Vec<JobId> = self
            .state
            .jobs
            .iter()
            .filter(|entry| {
                let j = entry.value();
                j.state == JobState::Dlq && j.updated_at < before
            })
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len() as u64;
        for id in to_remove {
            self.state.jobs.remove(&id);
            self.mark_dirty(id, DirtyKind::Delete);
        }
        Ok(count)
    }

    // ── Cron schedules ──────────────────────────────────────────────────

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        self.ensure_loaded().await?;
        {
            let mut map = self.state.schedules.write().unwrap();
            map.insert(schedule.name.clone(), schedule.clone());
        }
        self.mark_schedule_dirty(&schedule.name);
        Ok(())
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        self.ensure_loaded().await?;
        let map = self.state.schedules.read().unwrap();
        let active = map.values().filter(|s| !s.paused).cloned().collect();
        Ok(active)
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        self.ensure_loaded().await?;
        {
            let mut map = self.state.schedules.write().unwrap();
            map.remove(name);
        }
        self.mark_schedule_dirty(name);
        Ok(())
    }

    async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>> {
        self.ensure_loaded().await?;
        let map = self.state.schedules.read().unwrap();
        Ok(map.get(name).cloned())
    }

    async fn list_all_schedules(&self) -> Result<Vec<Schedule>> {
        self.ensure_loaded().await?;
        let map = self.state.schedules.read().unwrap();
        Ok(map.values().cloned().collect())
    }

    // ── Discovery ────────────────────────────────────────────────────────

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        self.ensure_loaded().await?;
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for entry in self.state.jobs.iter() {
            names.insert(entry.value().queue.clone());
        }
        Ok(names.into_iter().collect())
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        self.ensure_loaded().await?;
        for entry in self.state.jobs.iter() {
            let job = entry.value();
            if job.queue == queue
                && job.unique_key.as_deref() == Some(key)
                && !matches!(
                    job.state,
                    JobState::Completed | JobState::Dlq | JobState::Cancelled
                )
            {
                return Ok(Some(job.clone()));
            }
        }
        Ok(None)
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        self.ensure_loaded().await?;
        Ok(self
            .state
            .jobs
            .iter()
            .filter(|entry| entry.value().state == JobState::Active)
            .map(|entry| entry.value().clone())
            .collect())
    }
}
