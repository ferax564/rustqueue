//! Write-coalescing wrapper around [`RedbStorage`].
//!
//! Buffers single `insert_job` and `complete_job` calls, flushing them in
//! batches via `insert_jobs_batch` / `complete_jobs_batch` on the inner
//! backend. A background task flushes on a configurable interval (default
//! 10 ms) or when the buffer reaches `max_batch` entries.
//!
//! All other trait methods delegate directly to the inner storage.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify, oneshot};

use crate::engine::models::{Job, JobId, QueueCounts, Schedule};
use crate::storage::{CompleteJobOutcome, StorageBackend};

/// Configuration for the write coalescing layer.
#[derive(Debug, Clone)]
pub struct BufferedRedbConfig {
    /// Flush interval in milliseconds.
    pub interval_ms: u64,
    /// Maximum buffered operations before triggering a flush.
    pub max_batch: usize,
}

impl Default for BufferedRedbConfig {
    fn default() -> Self {
        Self {
            interval_ms: 10,
            max_batch: 100,
        }
    }
}

struct PendingInsert {
    job: Job,
    tx: oneshot::Sender<Result<JobId>>,
}

struct PendingComplete {
    id: JobId,
    result: Option<serde_json::Value>,
    tx: oneshot::Sender<Result<CompleteJobOutcome>>,
}

/// Write-coalescing wrapper that batches `insert_job` and `complete_job` calls.
pub struct BufferedRedbStorage {
    inner: Arc<dyn StorageBackend>,
    inserts: Arc<Mutex<Vec<PendingInsert>>>,
    completes: Arc<Mutex<Vec<PendingComplete>>>,
    notify: Arc<Notify>,
    max_batch: usize,
    _flush_handle: tokio::task::JoinHandle<()>,
}

impl BufferedRedbStorage {
    /// Wrap an existing storage backend with write coalescing.
    pub fn new(inner: Arc<dyn StorageBackend>, config: BufferedRedbConfig) -> Self {
        let inserts: Arc<Mutex<Vec<PendingInsert>>> = Arc::new(Mutex::new(Vec::new()));
        let completes: Arc<Mutex<Vec<PendingComplete>>> = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let max_batch = config.max_batch;

        let flush_handle = tokio::spawn(Self::flush_loop(
            Arc::clone(&inner),
            Arc::clone(&inserts),
            Arc::clone(&completes),
            Arc::clone(&notify),
            config.interval_ms,
        ));

        Self {
            inner,
            inserts,
            completes,
            notify,
            max_batch,
            _flush_handle: flush_handle,
        }
    }

    async fn flush_loop(
        inner: Arc<dyn StorageBackend>,
        inserts: Arc<Mutex<Vec<PendingInsert>>>,
        completes: Arc<Mutex<Vec<PendingComplete>>>,
        notify: Arc<Notify>,
        interval_ms: u64,
    ) {
        let interval = tokio::time::Duration::from_millis(interval_ms);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = notify.notified() => {}
            }
            Self::flush_once(&inner, &inserts, &completes).await;
        }
    }

    async fn flush_once(
        inner: &Arc<dyn StorageBackend>,
        inserts: &Arc<Mutex<Vec<PendingInsert>>>,
        completes: &Arc<Mutex<Vec<PendingComplete>>>,
    ) {
        // Drain inserts
        let pending_inserts: Vec<PendingInsert> = {
            let mut guard = inserts.lock().await;
            std::mem::take(&mut *guard)
        };

        if !pending_inserts.is_empty() {
            let jobs: Vec<Job> = pending_inserts.iter().map(|p| p.job.clone()).collect();
            let result = inner.insert_jobs_batch(&jobs).await;

            match result {
                Ok(ids) => {
                    for (pending, id) in pending_inserts.into_iter().zip(ids) {
                        let _ = pending.tx.send(Ok(id));
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    for pending in pending_inserts {
                        let _ = pending.tx.send(Err(anyhow::anyhow!("{}", msg)));
                    }
                }
            }
        }

        // Drain completes
        let pending_completes: Vec<PendingComplete> = {
            let mut guard = completes.lock().await;
            std::mem::take(&mut *guard)
        };

        if !pending_completes.is_empty() {
            let items: Vec<(JobId, Option<serde_json::Value>)> = pending_completes
                .iter()
                .map(|p| (p.id, p.result.clone()))
                .collect();
            let result = inner.complete_jobs_batch(&items).await;

            match result {
                Ok(outcomes) => {
                    for (pending, outcome) in pending_completes.into_iter().zip(outcomes) {
                        let _ = pending.tx.send(Ok(outcome));
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    for pending in pending_completes {
                        let _ = pending.tx.send(Err(anyhow::anyhow!("{}", msg)));
                    }
                }
            }
        }
    }

    /// Flush all pending inserts immediately (used by dequeue to ensure visibility).
    async fn flush_inserts(&self) {
        let pending: Vec<PendingInsert> = {
            let mut guard = self.inserts.lock().await;
            std::mem::take(&mut *guard)
        };

        if pending.is_empty() {
            return;
        }

        let jobs: Vec<Job> = pending.iter().map(|p| p.job.clone()).collect();
        let result = self.inner.insert_jobs_batch(&jobs).await;

        match result {
            Ok(ids) => {
                for (p, id) in pending.into_iter().zip(ids) {
                    let _ = p.tx.send(Ok(id));
                }
            }
            Err(e) => {
                let msg = e.to_string();
                for p in pending {
                    let _ = p.tx.send(Err(anyhow::anyhow!("{}", msg)));
                }
            }
        }
    }
}

#[async_trait]
impl StorageBackend for BufferedRedbStorage {
    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let (tx, rx) = oneshot::channel();
        let should_notify = {
            let mut guard = self.inserts.lock().await;
            guard.push(PendingInsert {
                job: job.clone(),
                tx,
            });
            guard.len() >= self.max_batch
        };
        if should_notify {
            self.notify.notify_one();
        }
        rx.await
            .map_err(|_| anyhow::anyhow!("flush task dropped"))?
    }

    // Batch inserts bypass the buffer (already batched)
    async fn insert_jobs_batch(&self, jobs: &[Job]) -> Result<Vec<JobId>> {
        self.flush_inserts().await;
        self.inner.insert_jobs_batch(jobs).await
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        self.inner.get_job(id).await
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        self.inner.update_job(job).await
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        self.inner.delete_job(id).await
    }

    async fn complete_job(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<CompleteJobOutcome> {
        let (tx, rx) = oneshot::channel();
        let should_notify = {
            let mut guard = self.completes.lock().await;
            guard.push(PendingComplete { id, result, tx });
            guard.len() >= self.max_batch
        };
        if should_notify {
            self.notify.notify_one();
        }
        rx.await
            .map_err(|_| anyhow::anyhow!("flush task dropped"))?
    }

    // Batch completes bypass the buffer
    async fn complete_jobs_batch(
        &self,
        items: &[(JobId, Option<serde_json::Value>)],
    ) -> Result<Vec<CompleteJobOutcome>> {
        self.inner.complete_jobs_batch(items).await
    }

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        // Flush pending inserts so freshly-pushed jobs are visible
        self.flush_inserts().await;
        self.inner.dequeue(queue, count).await
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        self.inner.get_queue_counts(queue).await
    }

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        self.inner.get_ready_scheduled(now).await
    }

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        self.inner.move_to_dlq(job, reason).await
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        self.inner.get_dlq_jobs(queue, limit).await
    }

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.inner.remove_completed_before(before).await
    }

    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.inner.remove_failed_before(before).await
    }

    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64> {
        self.inner.remove_dlq_before(before).await
    }

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        self.inner.upsert_schedule(schedule).await
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        self.inner.get_active_schedules().await
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        self.inner.delete_schedule(name).await
    }

    async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>> {
        self.inner.get_schedule(name).await
    }

    async fn list_all_schedules(&self) -> Result<Vec<Schedule>> {
        self.inner.list_all_schedules().await
    }

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        self.inner.list_queue_names().await
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        self.inner.get_job_by_unique_key(queue, key).await
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        self.inner.get_active_jobs().await
    }

    async fn get_jobs_by_flow_id(&self, flow_id: &str) -> Result<Vec<Job>> {
        self.inner.get_jobs_by_flow_id(flow_id).await
    }
}
