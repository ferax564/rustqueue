//! Embedded storage backend powered by [redb](https://docs.rs/redb).
//!
//! Uses two tables with byte key/value pairs:
//! - `JOBS_TABLE` — job ID (16 bytes UUID) -> JSON-serialized `Job`
//! - `SCHEDULES_TABLE` — schedule name (UTF-8 bytes) -> JSON-serialized `Schedule`

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redb::{Database, Durability, ReadableTable, ReadableTableMetadata, Table, TableDefinition};

use crate::engine::models::{Job, JobId, JobState, QueueCounts, Schedule};
use crate::storage::{CompleteJobOutcome, StorageBackend};

/// Main job storage: key = UUID bytes (16), value = JSON bytes.
const JOBS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("jobs");

/// Secondary index: queue/state/priority/created_at/job_id -> job_id
const JOBS_QUEUE_STATE_PRIORITY_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("jobs_queue_state_priority_idx");

/// Secondary index: state/updated_at/job_id -> job_id
const JOBS_STATE_UPDATED_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("jobs_state_updated_idx");

/// Secondary index: queue/unique_key -> job_id (only non-terminal states)
const JOBS_UNIQUE_KEY_INDEX: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("jobs_unique_key_idx");

/// Schedule storage: key = schedule name bytes, value = JSON bytes.
const SCHEDULES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("schedules");

/// Durability level for redb write transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedbDurability {
    /// Do not persist commits unless followed by a higher durability commit.
    ///
    /// This yields the highest throughput but weak crash durability.
    None,
    /// Commit returns only after data is persisted to disk.
    #[default]
    Immediate,
    /// Commit queues persistence and returns earlier for higher throughput.
    Eventual,
}

fn state_code(state: JobState) -> u8 {
    match state {
        JobState::Created => 0,
        JobState::Waiting => 1,
        JobState::Delayed => 2,
        JobState::Active => 3,
        JobState::Completed => 4,
        JobState::Failed => 5,
        JobState::Dlq => 6,
        JobState::Cancelled => 7,
        JobState::Blocked => 8,
    }
}

fn encode_i64_lex(value: i64) -> [u8; 8] {
    ((value as u64) ^ 0x8000_0000_0000_0000).to_be_bytes()
}

fn encode_priority_desc(priority: i32) -> [u8; 4] {
    let shifted = (priority as u32) ^ 0x8000_0000;
    (!shifted).to_be_bytes()
}

fn queue_state_prefix(queue: &str, state: JobState) -> Vec<u8> {
    let queue_bytes = queue.as_bytes();
    let mut key = Vec::with_capacity(4 + queue_bytes.len() + 1);
    key.extend_from_slice(&(queue_bytes.len() as u32).to_be_bytes());
    key.extend_from_slice(queue_bytes);
    key.push(state_code(state));
    key
}

fn state_prefix(state: JobState) -> Vec<u8> {
    vec![state_code(state)]
}

fn queue_state_priority_key(job: &Job) -> Vec<u8> {
    let queue_bytes = job.queue.as_bytes();
    let mut key = Vec::with_capacity(4 + queue_bytes.len() + 1 + 4 + 8 + 16);
    key.extend_from_slice(&(queue_bytes.len() as u32).to_be_bytes());
    key.extend_from_slice(queue_bytes);
    key.push(state_code(job.state));
    key.extend_from_slice(&encode_priority_desc(job.priority));
    key.extend_from_slice(&encode_i64_lex(job.created_at.timestamp_micros()));
    key.extend_from_slice(job.id.as_bytes());
    key
}

fn state_updated_key(job: &Job) -> [u8; 25] {
    let mut key = [0u8; 25];
    key[0] = state_code(job.state);
    key[1..9].copy_from_slice(&encode_i64_lex(job.updated_at.timestamp_micros()));
    key[9..25].copy_from_slice(job.id.as_bytes());
    key
}

fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    for i in (0..end.len()).rev() {
        if end[i] != 0xFF {
            end[i] += 1;
            end.truncate(i + 1);
            return Some(end);
        }
    }
    None
}

fn is_terminal_state(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Dlq | JobState::Cancelled
    )
}

fn unique_key_index_key(queue: &str, unique_key: &str) -> Vec<u8> {
    let queue_bytes = queue.as_bytes();
    let key_bytes = unique_key.as_bytes();
    let mut key = Vec::with_capacity(4 + queue_bytes.len() + key_bytes.len());
    key.extend_from_slice(&(queue_bytes.len() as u32).to_be_bytes());
    key.extend_from_slice(queue_bytes);
    key.extend_from_slice(key_bytes);
    key
}

fn decode_i64_lex(bytes: &[u8; 8]) -> i64 {
    let val = u64::from_be_bytes(*bytes);
    (val ^ 0x8000_0000_0000_0000) as i64
}

fn parse_index_job_id(bytes: &[u8]) -> Result<JobId> {
    JobId::from_slice(bytes).context("invalid job id bytes in index")
}

fn insert_job_indexes(
    queue_index: &mut Table<'_, &[u8], &[u8]>,
    state_index: &mut Table<'_, &[u8], &[u8]>,
    unique_key_index: &mut Table<'_, &[u8], &[u8]>,
    job: &Job,
) -> Result<()> {
    let queue_key = queue_state_priority_key(job);
    let state_key = state_updated_key(job);
    let id = job.id.as_bytes().as_slice();

    queue_index.insert(queue_key.as_slice(), id)?;
    state_index.insert(state_key.as_slice(), id)?;

    if let Some(ref ukey) = job.unique_key {
        if !is_terminal_state(job.state) {
            let uk_key = unique_key_index_key(&job.queue, ukey);
            unique_key_index.insert(uk_key.as_slice(), id)?;
        }
    }
    Ok(())
}

fn remove_job_indexes(
    queue_index: &mut Table<'_, &[u8], &[u8]>,
    state_index: &mut Table<'_, &[u8], &[u8]>,
    unique_key_index: &mut Table<'_, &[u8], &[u8]>,
    job: &Job,
) -> Result<()> {
    let queue_key = queue_state_priority_key(job);
    let state_key = state_updated_key(job);

    queue_index.remove(queue_key.as_slice())?;
    state_index.remove(state_key.as_slice())?;

    if let Some(ref ukey) = job.unique_key {
        let uk_key = unique_key_index_key(&job.queue, ukey);
        unique_key_index.remove(uk_key.as_slice())?;
    }
    Ok(())
}

fn complete_job_in_tables(
    jobs: &mut Table<'_, &[u8], &[u8]>,
    queue_index: &mut Table<'_, &[u8], &[u8]>,
    state_index: &mut Table<'_, &[u8], &[u8]>,
    unique_key_index: &mut Table<'_, &[u8], &[u8]>,
    id: JobId,
    result: Option<serde_json::Value>,
) -> Result<CompleteJobOutcome> {
    let key = id.as_bytes().as_slice();
    let mut job: Job = {
        let stored = jobs.get(key)?;
        let Some(stored) = stored else {
            return Ok(CompleteJobOutcome::NotFound);
        };
        serde_json::from_slice(stored.value())
            .context("failed to deserialize existing job during complete_job")?
    };

    if job.state != JobState::Active {
        return Ok(CompleteJobOutcome::InvalidState(job.state));
    }

    let previous = job.clone();
    let now = Utc::now();
    job.state = JobState::Completed;
    job.completed_at = Some(now);
    job.updated_at = now;
    job.result = result;

    remove_job_indexes(queue_index, state_index, unique_key_index, &previous)?;

    if job.remove_on_complete {
        jobs.remove(key)?;
    } else {
        let value = serde_json::to_vec(&job)
            .context("failed to serialize completed job during complete_job")?;
        jobs.insert(key, value.as_slice())?;
        insert_job_indexes(queue_index, state_index, unique_key_index, &job)?;
    }

    Ok(CompleteJobOutcome::Completed(Box::new(job)))
}

/// Embedded storage backend using redb — a pure-Rust, ACID, embedded key-value store.
///
/// redb is synchronous, so each storage call is run in `spawn_blocking` to avoid
/// blocking Tokio runtime worker threads.
pub struct RedbStorage {
    db: Arc<Database>,
    durability: RedbDurability,
}

impl RedbStorage {
    /// Create or open a redb database at the given path.
    ///
    /// On first open the required tables are created automatically.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::new_with_durability(path, RedbDurability::Immediate)
    }

    /// Create or open a redb database at the given path with explicit write durability.
    pub fn new_with_durability(path: impl AsRef<Path>, durability: RedbDurability) -> Result<Self> {
        let db = Arc::new(
            Database::create(path.as_ref())
                .with_context(|| format!("failed to open redb at {:?}", path.as_ref()))?,
        );

        // Ensure tables exist by opening a write transaction.
        let write_txn = Self::begin_write_txn(&db, durability)?;
        {
            let _jobs = write_txn.open_table(JOBS_TABLE)?;
            let _queue_state_priority = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
            let _state_updated = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
            let _unique_key = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;
            let _schedules = write_txn.open_table(SCHEDULES_TABLE)?;
        }
        write_txn.commit()?;
        Self::rebuild_indexes_if_needed(&db, durability)?;

        Ok(Self { db, durability })
    }

    fn begin_write_txn(
        db: &Arc<Database>,
        durability: RedbDurability,
    ) -> Result<redb::WriteTransaction> {
        let mut write_txn = db.begin_write()?;
        match durability {
            RedbDurability::None => write_txn.set_durability(Durability::None),
            RedbDurability::Immediate => {}
            RedbDurability::Eventual => write_txn.set_durability(Durability::Eventual),
        }
        Ok(write_txn)
    }

    fn rebuild_indexes_if_needed(db: &Arc<Database>, durability: RedbDurability) -> Result<()> {
        let should_rebuild = {
            let read_txn = db.begin_read()?;
            let jobs = read_txn.open_table(JOBS_TABLE)?;
            let queue_index = read_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
            let state_index = read_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;

            let jobs_len = jobs.len()?;
            jobs_len > 0 && (queue_index.len()? != jobs_len || state_index.len()? != jobs_len)
        };

        if !should_rebuild {
            return Ok(());
        }

        let write_txn = Self::begin_write_txn(db, durability)?;
        {
            let jobs = write_txn.open_table(JOBS_TABLE)?;
            let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
            let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
            let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

            queue_index.retain(|_, _| false)?;
            state_index.retain(|_, _| false)?;
            unique_key_index.retain(|_, _| false)?;

            for entry in jobs.iter()? {
                let (_, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())
                    .context("failed to deserialize job while rebuilding indexes")?;
                insert_job_indexes(
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    &job,
                )?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Run a synchronous redb operation on the blocking pool.
    async fn run_blocking<T, F>(&self, operation: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Database>) -> Result<T> + Send + 'static,
    {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || f(db))
            .await
            .with_context(|| format!("redb {operation} task failed"))?
    }
}

#[async_trait]
impl StorageBackend for RedbStorage {
    // ── Job CRUD ────────────────────────────────────────────────────────

    async fn insert_job(&self, job: &Job) -> Result<JobId> {
        let job = job.clone();
        let durability = self.durability;
        self.run_blocking("insert_job", move |db| {
            let id = job.id;
            let key = id.as_bytes().as_slice();
            let value = serde_json::to_vec(&job).context("failed to serialize job")?;

            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut jobs = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                jobs.insert(key, value.as_slice())?;
                insert_job_indexes(
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    &job,
                )?;
            }
            write_txn.commit()?;
            Ok(id)
        })
        .await
    }

    async fn insert_jobs_batch(&self, jobs: &[Job]) -> Result<Vec<JobId>> {
        let jobs = jobs.to_vec();
        let durability = self.durability;
        self.run_blocking("insert_jobs_batch", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let ids = {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;
                let mut ids = Vec::with_capacity(jobs.len());
                for job in &jobs {
                    let id = job.id;
                    let key = id.as_bytes().as_slice();
                    let value = serde_json::to_vec(job).context("failed to serialize job")?;
                    jobs_table.insert(key, value.as_slice())?;
                    insert_job_indexes(
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        job,
                    )?;
                    ids.push(id);
                }
                ids
            };
            write_txn.commit()?;
            Ok(ids)
        })
        .await
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        self.run_blocking("get_job", move |db| {
            let key = id.as_bytes().as_slice();
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(JOBS_TABLE)?;

            match table.get(key)? {
                Some(value) => {
                    let job: Job = serde_json::from_slice(value.value())
                        .context("failed to deserialize job")?;
                    Ok(Some(job))
                }
                None => Ok(None),
            }
        })
        .await
    }

    async fn update_job(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        let durability = self.durability;
        self.run_blocking("update_job", move |db| {
            let key = job.id.as_bytes().as_slice();
            let value = serde_json::to_vec(&job).context("failed to serialize job")?;

            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut jobs = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let previous = jobs
                    .get(key)?
                    .map(|existing| {
                        serde_json::from_slice::<Job>(existing.value())
                            .context("failed to deserialize existing job during update")
                    })
                    .transpose()?;

                if let Some(previous) = previous.as_ref() {
                    remove_job_indexes(
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        previous,
                    )?;
                }

                jobs.insert(key, value.as_slice())?;
                insert_job_indexes(
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    &job,
                )?;
            }
            write_txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn delete_job(&self, id: JobId) -> Result<()> {
        let durability = self.durability;
        self.run_blocking("delete_job", move |db| {
            let key = id.as_bytes().as_slice();
            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut jobs = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let previous = jobs
                    .get(key)?
                    .map(|existing| {
                        serde_json::from_slice::<Job>(existing.value())
                            .context("failed to deserialize existing job during delete")
                    })
                    .transpose()?;

                jobs.remove(key)?;
                if let Some(previous) = previous.as_ref() {
                    remove_job_indexes(
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        previous,
                    )?;
                }
            }
            write_txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn complete_job(
        &self,
        id: JobId,
        result: Option<serde_json::Value>,
    ) -> Result<CompleteJobOutcome> {
        let durability = self.durability;
        self.run_blocking("complete_job", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let outcome = {
                let mut jobs = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;
                complete_job_in_tables(
                    &mut jobs,
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    id,
                    result,
                )?
            };
            write_txn.commit()?;
            Ok(outcome)
        })
        .await
    }

    async fn complete_jobs_batch(
        &self,
        items: &[(JobId, Option<serde_json::Value>)],
    ) -> Result<Vec<CompleteJobOutcome>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let items = items.to_vec();
        let durability = self.durability;
        self.run_blocking("complete_jobs_batch", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let outcomes = {
                let mut jobs = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;
                let mut outcomes = Vec::with_capacity(items.len());

                for (id, result) in items {
                    outcomes.push(complete_job_in_tables(
                        &mut jobs,
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        id,
                        result,
                    )?);
                }
                outcomes
            };
            write_txn.commit()?;
            Ok(outcomes)
        })
        .await
    }

    // ── Queue operations ────────────────────────────────────────────────

    async fn dequeue(&self, queue: &str, count: u32) -> Result<Vec<Job>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let queue = queue.to_string();
        let durability = self.durability;
        self.run_blocking("dequeue", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let result = {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let waiting_prefix = queue_state_prefix(&queue, JobState::Waiting);
                let waiting_end = prefix_upper_bound(&waiting_prefix);
                let mut selected = Vec::with_capacity(count as usize);
                let mut stale_queue_keys = Vec::new();

                let range = if let Some(ref end) = waiting_end {
                    queue_index.range(waiting_prefix.as_slice()..end.as_slice())?
                } else {
                    queue_index.range(waiting_prefix.as_slice()..)?
                };

                for entry in range {
                    let (index_key, index_value) = entry?;
                    let id = parse_index_job_id(index_value.value())?;
                    let stored = jobs_table.get(id.as_bytes().as_slice())?;
                    let Some(stored) = stored else {
                        stale_queue_keys.push(index_key.value().to_vec());
                        continue;
                    };

                    let job: Job = serde_json::from_slice(stored.value())
                        .context("failed to deserialize indexed waiting job during dequeue")?;
                    if job.queue != queue || job.state != JobState::Waiting {
                        stale_queue_keys.push(index_key.value().to_vec());
                        continue;
                    }

                    selected.push(job);
                    if selected.len() >= count as usize {
                        break;
                    }
                }

                for key in stale_queue_keys {
                    queue_index.remove(key.as_slice())?;
                }

                let now = Utc::now();
                let mut activated = Vec::with_capacity(selected.len());
                for mut job in selected {
                    let previous = job.clone();
                    job.state = JobState::Active;
                    job.started_at = Some(now);
                    job.updated_at = now;

                    let key = job.id.as_bytes().as_slice();
                    let value = serde_json::to_vec(&job)?;
                    jobs_table.insert(key, value.as_slice())?;

                    remove_job_indexes(
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        &previous,
                    )?;
                    insert_job_indexes(
                        &mut queue_index,
                        &mut state_index,
                        &mut unique_key_index,
                        &job,
                    )?;
                    activated.push(job);
                }

                activated
            };
            write_txn.commit()?;
            Ok(result)
        })
        .await
    }

    async fn get_queue_counts(&self, queue: &str) -> Result<QueueCounts> {
        let queue = queue.to_string();
        self.run_blocking("get_queue_counts", move |db| {
            let read_txn = db.begin_read()?;
            let queue_index = read_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;

            let count_for_state = |state: JobState| -> Result<u64> {
                let prefix = queue_state_prefix(&queue, state);
                let end = prefix_upper_bound(&prefix);
                let range = if let Some(ref end) = end {
                    queue_index.range(prefix.as_slice()..end.as_slice())?
                } else {
                    queue_index.range(prefix.as_slice()..)?
                };

                let mut count = 0u64;
                for entry in range {
                    let _ = entry?;
                    count += 1;
                }
                Ok(count)
            };

            let mut counts = QueueCounts {
                waiting: count_for_state(JobState::Created)? + count_for_state(JobState::Waiting)?,
                active: count_for_state(JobState::Active)?,
                delayed: count_for_state(JobState::Delayed)?,
                completed: count_for_state(JobState::Completed)?,
                failed: count_for_state(JobState::Failed)?,
                dlq: count_for_state(JobState::Dlq)?,
                blocked: count_for_state(JobState::Blocked)?,
            };

            // Keep compatibility with callers expecting non-negative counters.
            if counts.waiting == 0
                && counts.active == 0
                && counts.delayed == 0
                && counts.completed == 0
                && counts.failed == 0
                && counts.dlq == 0
                && counts.blocked == 0
            {
                counts = QueueCounts::default();
            }
            Ok(counts)
        })
        .await
    }

    // ── Scheduled jobs ──────────────────────────────────────────────────

    async fn get_ready_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        self.run_blocking("get_ready_scheduled", move |db| {
            let read_txn = db.begin_read()?;
            let jobs_table = read_txn.open_table(JOBS_TABLE)?;
            let state_index = read_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;

            let mut ready = Vec::new();

            let delayed_prefix = state_prefix(JobState::Delayed);
            let delayed_end = prefix_upper_bound(&delayed_prefix);
            let range = if let Some(ref end) = delayed_end {
                state_index.range(delayed_prefix.as_slice()..end.as_slice())?
            } else {
                state_index.range(delayed_prefix.as_slice()..)?
            };

            for entry in range {
                let (_, value) = entry?;
                let id = parse_index_job_id(value.value())?;

                let stored = jobs_table.get(id.as_bytes().as_slice())?;
                let Some(stored) = stored else {
                    continue;
                };
                let job: Job = serde_json::from_slice(stored.value())
                    .context("failed to deserialize delayed job from index")?;

                if job.state != JobState::Delayed {
                    continue;
                }
                if let Some(delay_until) = job.delay_until {
                    if delay_until <= now {
                        ready.push(job);
                    }
                }
            }
            Ok(ready)
        })
        .await
    }

    // ── DLQ ─────────────────────────────────────────────────────────────

    async fn move_to_dlq(&self, job: &Job, reason: &str) -> Result<()> {
        let job = job.clone();
        let reason = reason.to_string();
        let durability = self.durability;
        self.run_blocking("move_to_dlq", move |db| {
            let mut updated = job.clone();
            updated.state = JobState::Dlq;
            updated.last_error = Some(reason);
            updated.updated_at = Utc::now();

            let key = updated.id.as_bytes().as_slice();
            let value = serde_json::to_vec(&updated)?;

            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let previous = jobs_table
                    .get(key)?
                    .map(|existing| {
                        serde_json::from_slice::<Job>(existing.value())
                            .context("failed to deserialize existing job during move_to_dlq")
                    })
                    .transpose()?
                    .unwrap_or(job);

                remove_job_indexes(
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    &previous,
                )?;
                jobs_table.insert(key, value.as_slice())?;
                insert_job_indexes(
                    &mut queue_index,
                    &mut state_index,
                    &mut unique_key_index,
                    &updated,
                )?;
            }
            write_txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn get_dlq_jobs(&self, queue: &str, limit: u32) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let queue = queue.to_string();
        self.run_blocking("get_dlq_jobs", move |db| {
            let read_txn = db.begin_read()?;
            let jobs_table = read_txn.open_table(JOBS_TABLE)?;
            let queue_index = read_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;

            let mut dlq_jobs = Vec::new();
            let dlq_prefix = queue_state_prefix(&queue, JobState::Dlq);
            let dlq_end = prefix_upper_bound(&dlq_prefix);
            let range = if let Some(ref end) = dlq_end {
                queue_index.range(dlq_prefix.as_slice()..end.as_slice())?
            } else {
                queue_index.range(dlq_prefix.as_slice()..)?
            };

            for entry in range {
                let (_, value) = entry?;
                let id = parse_index_job_id(value.value())?;
                let stored = jobs_table.get(id.as_bytes().as_slice())?;
                let Some(stored) = stored else {
                    continue;
                };
                let job: Job = serde_json::from_slice(stored.value())
                    .context("failed to deserialize dlq job from index")?;
                if job.state != JobState::Dlq || job.queue != queue {
                    continue;
                }
                dlq_jobs.push(job);
                if dlq_jobs.len() >= limit as usize {
                    break;
                }
            }
            Ok(dlq_jobs)
        })
        .await
    }

    // ── Cleanup ─────────────────────────────────────────────────────────

    async fn remove_completed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let durability = self.durability;
        self.run_blocking("remove_completed_before", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let removed = {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                // Scan only Completed entries from the state index (O(K) not O(N))
                let completed_prefix = state_prefix(JobState::Completed);
                let completed_end = prefix_upper_bound(&completed_prefix);

                let range = if let Some(ref end) = completed_end {
                    state_index.range(completed_prefix.as_slice()..end.as_slice())?
                } else {
                    state_index.range(completed_prefix.as_slice()..)?
                };

                // Collect candidate IDs — we check completed_at on the actual job
                // because updated_at and completed_at can differ in edge cases.
                let mut candidate_ids: Vec<JobId> = Vec::new();
                for entry in range {
                    let (_, index_value) = entry?;
                    let id = parse_index_job_id(index_value.value())?;
                    candidate_ids.push(id);
                }

                let mut count = 0u64;
                for id in &candidate_ids {
                    let job = {
                        let stored = jobs_table.get(id.as_bytes().as_slice())?;
                        stored
                            .map(|s| serde_json::from_slice::<Job>(s.value()))
                            .transpose()?
                    };
                    if let Some(job) = job {
                        if job.state == JobState::Completed {
                            if let Some(completed_at) = job.completed_at {
                                if completed_at < before {
                                    remove_job_indexes(
                                        &mut queue_index,
                                        &mut state_index,
                                        &mut unique_key_index,
                                        &job,
                                    )?;
                                    jobs_table.remove(id.as_bytes().as_slice())?;
                                    count += 1;
                                }
                            }
                        }
                    }
                }
                count
            };
            write_txn.commit()?;
            Ok(removed)
        })
        .await
    }

    async fn remove_failed_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let durability = self.durability;
        self.run_blocking("remove_failed_before", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let removed = {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let failed_prefix = state_prefix(JobState::Failed);
                let failed_end = prefix_upper_bound(&failed_prefix);
                let cutoff_micros = before.timestamp_micros();

                let range = if let Some(ref end) = failed_end {
                    state_index.range(failed_prefix.as_slice()..end.as_slice())?
                } else {
                    state_index.range(failed_prefix.as_slice()..)?
                };

                let mut to_remove_ids: Vec<JobId> = Vec::new();
                for entry in range {
                    let (index_key, index_value) = entry?;
                    let key_bytes = index_key.value();
                    if key_bytes.len() >= 9 {
                        let updated_micros = decode_i64_lex(key_bytes[1..9].try_into().unwrap());
                        if updated_micros >= cutoff_micros {
                            break;
                        }
                    }
                    let id = parse_index_job_id(index_value.value())?;
                    to_remove_ids.push(id);
                }

                let count = to_remove_ids.len() as u64;
                for id in &to_remove_ids {
                    let job = {
                        let stored = jobs_table.get(id.as_bytes().as_slice())?;
                        stored
                            .map(|s| serde_json::from_slice::<Job>(s.value()))
                            .transpose()?
                    };
                    if let Some(job) = job {
                        remove_job_indexes(
                            &mut queue_index,
                            &mut state_index,
                            &mut unique_key_index,
                            &job,
                        )?;
                        jobs_table.remove(id.as_bytes().as_slice())?;
                    }
                }
                count
            };
            write_txn.commit()?;
            Ok(removed)
        })
        .await
    }

    async fn remove_dlq_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let durability = self.durability;
        self.run_blocking("remove_dlq_before", move |db| {
            let write_txn = Self::begin_write_txn(&db, durability)?;
            let removed = {
                let mut jobs_table = write_txn.open_table(JOBS_TABLE)?;
                let mut queue_index = write_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
                let mut unique_key_index = write_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;

                let dlq_prefix = state_prefix(JobState::Dlq);
                let dlq_end = prefix_upper_bound(&dlq_prefix);
                let cutoff_micros = before.timestamp_micros();

                let range = if let Some(ref end) = dlq_end {
                    state_index.range(dlq_prefix.as_slice()..end.as_slice())?
                } else {
                    state_index.range(dlq_prefix.as_slice()..)?
                };

                let mut to_remove_ids: Vec<JobId> = Vec::new();
                for entry in range {
                    let (index_key, index_value) = entry?;
                    let key_bytes = index_key.value();
                    if key_bytes.len() >= 9 {
                        let updated_micros = decode_i64_lex(key_bytes[1..9].try_into().unwrap());
                        if updated_micros >= cutoff_micros {
                            break;
                        }
                    }
                    let id = parse_index_job_id(index_value.value())?;
                    to_remove_ids.push(id);
                }

                let count = to_remove_ids.len() as u64;
                for id in &to_remove_ids {
                    let job = {
                        let stored = jobs_table.get(id.as_bytes().as_slice())?;
                        stored
                            .map(|s| serde_json::from_slice::<Job>(s.value()))
                            .transpose()?
                    };
                    if let Some(job) = job {
                        remove_job_indexes(
                            &mut queue_index,
                            &mut state_index,
                            &mut unique_key_index,
                            &job,
                        )?;
                        jobs_table.remove(id.as_bytes().as_slice())?;
                    }
                }
                count
            };
            write_txn.commit()?;
            Ok(removed)
        })
        .await
    }

    // ── Cron schedules ──────────────────────────────────────────────────

    async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        let schedule = schedule.clone();
        let durability = self.durability;
        self.run_blocking("upsert_schedule", move |db| {
            let key = schedule.name.as_bytes();
            let value = serde_json::to_vec(&schedule).context("failed to serialize schedule")?;

            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut table = write_txn.open_table(SCHEDULES_TABLE)?;
                table.insert(key, value.as_slice())?;
            }
            write_txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn get_active_schedules(&self) -> Result<Vec<Schedule>> {
        self.run_blocking("get_active_schedules", move |db| {
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(SCHEDULES_TABLE)?;

            let mut schedules = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                let schedule: Schedule = serde_json::from_slice(value.value())?;
                if !schedule.paused {
                    schedules.push(schedule);
                }
            }
            Ok(schedules)
        })
        .await
    }

    async fn delete_schedule(&self, name: &str) -> Result<()> {
        let name = name.to_string();
        let durability = self.durability;
        self.run_blocking("delete_schedule", move |db| {
            let key = name.as_bytes();
            let write_txn = Self::begin_write_txn(&db, durability)?;
            {
                let mut table = write_txn.open_table(SCHEDULES_TABLE)?;
                table.remove(key)?;
            }
            write_txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn get_schedule(&self, name: &str) -> Result<Option<Schedule>> {
        let name = name.to_string();
        self.run_blocking("get_schedule", move |db| {
            let key = name.as_bytes();
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(SCHEDULES_TABLE)?;

            match table.get(key)? {
                Some(value) => {
                    let schedule: Schedule = serde_json::from_slice(value.value())
                        .context("failed to deserialize schedule")?;
                    Ok(Some(schedule))
                }
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_all_schedules(&self) -> Result<Vec<Schedule>> {
        self.run_blocking("list_all_schedules", move |db| {
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(SCHEDULES_TABLE)?;

            let mut schedules = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                let schedule: Schedule = serde_json::from_slice(value.value())?;
                schedules.push(schedule);
            }
            Ok(schedules)
        })
        .await
    }

    // ── Discovery ────────────────────────────────────────────────────────

    async fn list_queue_names(&self) -> Result<Vec<String>> {
        self.run_blocking("list_queue_names", move |db| {
            let read_txn = db.begin_read()?;
            let queue_index = read_txn.open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)?;

            let mut names = std::collections::BTreeSet::new();
            let mut prev_queue_bytes: Option<Vec<u8>> = None;

            for entry in queue_index.iter()? {
                let (key, _) = entry?;
                let key_bytes = key.value();
                if key_bytes.len() < 4 {
                    continue;
                }
                let queue_len =
                    u32::from_be_bytes([key_bytes[0], key_bytes[1], key_bytes[2], key_bytes[3]])
                        as usize;
                if key_bytes.len() < 4 + queue_len {
                    continue;
                }
                let queue_bytes = &key_bytes[4..4 + queue_len];

                // Keys are sorted by queue name, so skip duplicates cheaply
                if let Some(ref prev) = prev_queue_bytes {
                    if prev.as_slice() == queue_bytes {
                        continue;
                    }
                }
                prev_queue_bytes = Some(queue_bytes.to_vec());

                if let Ok(name) = std::str::from_utf8(queue_bytes) {
                    names.insert(name.to_string());
                }
            }
            Ok(names.into_iter().collect())
        })
        .await
    }

    async fn get_job_by_unique_key(&self, queue: &str, key: &str) -> Result<Option<Job>> {
        let queue = queue.to_string();
        let key = key.to_string();
        self.run_blocking("get_job_by_unique_key", move |db| {
            let read_txn = db.begin_read()?;
            let unique_index = read_txn.open_table(JOBS_UNIQUE_KEY_INDEX)?;
            let jobs_table = read_txn.open_table(JOBS_TABLE)?;

            let uk_key = unique_key_index_key(&queue, &key);
            match unique_index.get(uk_key.as_slice())? {
                Some(value) => {
                    let id = parse_index_job_id(value.value())?;
                    let stored = jobs_table.get(id.as_bytes().as_slice())?;
                    let Some(stored) = stored else {
                        return Ok(None);
                    };
                    let job: Job = serde_json::from_slice(stored.value())
                        .context("failed to deserialize job from unique key index")?;
                    // Verify non-terminal and matches
                    if !is_terminal_state(job.state)
                        && job.queue == queue
                        && job.unique_key.as_deref() == Some(key.as_str())
                    {
                        Ok(Some(job))
                    } else {
                        Ok(None)
                    }
                }
                None => Ok(None),
            }
        })
        .await
    }

    async fn get_jobs_by_flow_id(&self, flow_id: &str) -> Result<Vec<Job>> {
        let flow_id = flow_id.to_string();
        self.run_blocking("get_jobs_by_flow_id", move |db| {
            let read_txn = db.begin_read()?;
            let jobs_table = read_txn.open_table(JOBS_TABLE)?;
            let mut result = Vec::new();
            for entry in jobs_table.iter()? {
                let (_, value) = entry?;
                let job: Job = serde_json::from_slice(value.value())
                    .context("failed to deserialize job for flow_id lookup")?;
                if job.flow_id.as_deref() == Some(flow_id.as_str()) {
                    result.push(job);
                }
            }
            Ok(result)
        })
        .await
    }

    async fn get_active_jobs(&self) -> Result<Vec<Job>> {
        self.run_blocking("get_active_jobs", move |db| {
            let read_txn = db.begin_read()?;
            let jobs_table = read_txn.open_table(JOBS_TABLE)?;
            let state_index = read_txn.open_table(JOBS_STATE_UPDATED_INDEX)?;
            let mut active = Vec::new();

            let active_prefix = state_prefix(JobState::Active);
            let active_end = prefix_upper_bound(&active_prefix);
            let range = if let Some(ref end) = active_end {
                state_index.range(active_prefix.as_slice()..end.as_slice())?
            } else {
                state_index.range(active_prefix.as_slice()..)?
            };

            for entry in range {
                let (_, value) = entry?;
                let id = parse_index_job_id(value.value())?;
                let stored = jobs_table.get(id.as_bytes().as_slice())?;
                let Some(stored) = stored else {
                    continue;
                };
                let job: Job = serde_json::from_slice(stored.value())
                    .context("failed to deserialize active job from index")?;
                if job.state != JobState::Active {
                    continue;
                }
                active.push(job);
            }
            Ok(active)
        })
        .await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use redb::ReadableTableMetadata;
    use serde_json::json;
    use tempfile::NamedTempFile;

    /// Helper: create a temporary RedbStorage instance.
    fn temp_storage() -> RedbStorage {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        // Drop the NamedTempFile so redb can create its own file at the path.
        drop(tmp);
        RedbStorage::new(&path).unwrap()
    }

    /// Helper: create a test job in the given queue.
    fn test_job(queue: &str) -> Job {
        Job::new(queue, "test-job", json!({"key": "value"}))
    }

    // ── Task 2: Insert/Get ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_insert_and_get_job() {
        let storage = temp_storage();
        let job = test_job("emails");

        let id = storage.insert_job(&job).await.unwrap();
        assert_eq!(id, job.id);

        let retrieved = storage
            .get_job(id)
            .await
            .unwrap()
            .expect("job should exist");
        assert_eq!(retrieved.id, job.id);
        assert_eq!(retrieved.queue, "emails");
        assert_eq!(retrieved.name, "test-job");
        assert_eq!(retrieved.state, JobState::Waiting);
        assert_eq!(retrieved.data, json!({"key": "value"}));
        assert_eq!(retrieved.priority, 0);
        assert_eq!(retrieved.max_attempts, 3);
    }

    #[tokio::test]
    async fn test_get_nonexistent_job_returns_none() {
        let storage = temp_storage();
        let fake_id = uuid::Uuid::now_v7();
        let result = storage.get_job(fake_id).await.unwrap();
        assert!(result.is_none());
    }

    // ── Task 3: Update/Delete/Dequeue ───────────────────────────────────

    #[tokio::test]
    async fn test_update_job() {
        let storage = temp_storage();
        let mut job = test_job("emails");
        storage.insert_job(&job).await.unwrap();

        // Update state and attempt count.
        job.state = JobState::Active;
        job.attempt = 1;
        job.updated_at = Utc::now();
        storage.update_job(&job).await.unwrap();

        let retrieved = storage.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(retrieved.state, JobState::Active);
        assert_eq!(retrieved.attempt, 1);
    }

    #[tokio::test]
    async fn test_delete_job() {
        let storage = temp_storage();
        let job = test_job("emails");
        let id = storage.insert_job(&job).await.unwrap();

        storage.delete_job(id).await.unwrap();

        let result = storage.get_job(id).await.unwrap();
        assert!(result.is_none(), "deleted job should not be found");
    }

    #[tokio::test]
    async fn test_dequeue_returns_waiting_jobs_fifo() {
        let storage = temp_storage();

        // Insert two jobs with the same priority; earlier created_at should come first.
        let job1 = test_job("emails");
        let mut job2 = test_job("emails");
        // Ensure job2 has a later created_at.
        job2.created_at = job1.created_at + Duration::seconds(1);

        storage.insert_job(&job1).await.unwrap();
        storage.insert_job(&job2).await.unwrap();

        // Dequeue 1 — should get job1 (FIFO).
        let dequeued = storage.dequeue("emails", 1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(dequeued[0].id, job1.id);
        assert_eq!(dequeued[0].state, JobState::Active);
        assert!(dequeued[0].started_at.is_some());

        // Verify job1 is now Active in storage.
        let stored = storage.get_job(job1.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Active);
    }

    #[tokio::test]
    async fn test_dequeue_empty_queue_returns_empty() {
        let storage = temp_storage();
        let dequeued = storage.dequeue("nonexistent", 5).await.unwrap();
        assert!(dequeued.is_empty());
    }

    #[tokio::test]
    async fn test_dequeue_respects_priority() {
        let storage = temp_storage();

        let mut low = test_job("work");
        low.priority = 1;

        let mut high = test_job("work");
        high.priority = 10;
        // Give high a later created_at to prove priority wins over FIFO.
        high.created_at = low.created_at + Duration::seconds(5);

        storage.insert_job(&low).await.unwrap();
        storage.insert_job(&high).await.unwrap();

        let dequeued = storage.dequeue("work", 1).await.unwrap();
        assert_eq!(dequeued.len(), 1);
        assert_eq!(
            dequeued[0].id, high.id,
            "higher priority job should be dequeued first"
        );
        assert_eq!(dequeued[0].state, JobState::Active);
    }

    // ── Task 4: Counts/DLQ/Schedules/Cleanup ────────────────────────────

    #[tokio::test]
    async fn test_queue_counts() {
        let storage = temp_storage();

        // Insert jobs with various states.
        let mut j_waiting = test_job("q");
        j_waiting.state = JobState::Waiting;

        let mut j_active = test_job("q");
        j_active.state = JobState::Active;

        let mut j_completed = test_job("q");
        j_completed.state = JobState::Completed;

        let mut j_failed = test_job("q");
        j_failed.state = JobState::Failed;

        let mut j_delayed = test_job("q");
        j_delayed.state = JobState::Delayed;

        let mut j_dlq = test_job("q");
        j_dlq.state = JobState::Dlq;

        // A job in a different queue — should not be counted.
        let j_other = test_job("other");

        for job in [
            &j_waiting,
            &j_active,
            &j_completed,
            &j_failed,
            &j_delayed,
            &j_dlq,
            &j_other,
        ] {
            storage.insert_job(job).await.unwrap();
        }

        let counts = storage.get_queue_counts("q").await.unwrap();
        assert_eq!(counts.waiting, 1);
        assert_eq!(counts.active, 1);
        assert_eq!(counts.completed, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.delayed, 1);
        assert_eq!(counts.dlq, 1);
    }

    #[tokio::test]
    async fn test_move_to_dlq_and_retrieve() {
        let storage = temp_storage();

        let mut job = test_job("emails");
        job.state = JobState::Failed;
        storage.insert_job(&job).await.unwrap();

        storage
            .move_to_dlq(&job, "max retries exceeded")
            .await
            .unwrap();

        // Verify the job is now in DLQ state.
        let stored = storage.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(stored.state, JobState::Dlq);
        assert_eq!(stored.last_error.as_deref(), Some("max retries exceeded"));

        // Verify get_dlq_jobs returns it.
        let dlq_jobs = storage.get_dlq_jobs("emails", 10).await.unwrap();
        assert_eq!(dlq_jobs.len(), 1);
        assert_eq!(dlq_jobs[0].id, job.id);
    }

    #[tokio::test]
    async fn test_scheduled_jobs_ready() {
        let storage = temp_storage();

        let mut job = test_job("emails");
        job.state = JobState::Delayed;
        // Set delay_until to the past so it should be ready.
        job.delay_until = Some(Utc::now() - Duration::seconds(60));
        storage.insert_job(&job).await.unwrap();

        // Also insert a future-delayed job that should NOT be returned.
        let mut future_job = test_job("emails");
        future_job.state = JobState::Delayed;
        future_job.delay_until = Some(Utc::now() + Duration::hours(1));
        storage.insert_job(&future_job).await.unwrap();

        let ready = storage.get_ready_scheduled(Utc::now()).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, job.id);
    }

    #[tokio::test]
    async fn test_complete_jobs_batch() {
        let storage = temp_storage();

        let job1 = test_job("batch-ack");
        let job2 = test_job("batch-ack");
        storage.insert_job(&job1).await.unwrap();
        storage.insert_job(&job2).await.unwrap();

        let activated = storage.dequeue("batch-ack", 2).await.unwrap();
        assert_eq!(activated.len(), 2);

        let outcomes = storage
            .complete_jobs_batch(&[
                (job1.id, Some(json!({"result": 1}))),
                (job2.id, Some(json!({"result": 2}))),
            ])
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(matches!(outcomes[0], CompleteJobOutcome::Completed(_)));
        assert!(matches!(outcomes[1], CompleteJobOutcome::Completed(_)));

        let stored1 = storage.get_job(job1.id).await.unwrap().unwrap();
        let stored2 = storage.get_job(job2.id).await.unwrap().unwrap();
        assert_eq!(stored1.state, JobState::Completed);
        assert_eq!(stored2.state, JobState::Completed);
        assert_eq!(stored1.result, Some(json!({"result": 1})));
        assert_eq!(stored2.result, Some(json!({"result": 2})));
    }

    #[tokio::test]
    async fn test_upsert_and_get_schedules() {
        let storage = temp_storage();

        let now = Utc::now();
        let schedule = Schedule {
            name: "daily-report".to_string(),
            queue: "reports".to_string(),
            job_name: "generate-report".to_string(),
            job_data: json!({"type": "daily"}),
            job_options: None,
            cron_expr: Some("0 0 * * *".to_string()),
            every_ms: None,
            timezone: None,
            max_executions: None,
            execution_count: 0,
            paused: false,
            last_run_at: None,
            next_run_at: None,
            created_at: now,
            updated_at: now,
        };

        storage.upsert_schedule(&schedule).await.unwrap();

        let active = storage.get_active_schedules().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "daily-report");
        assert_eq!(active[0].queue, "reports");

        // Upsert again with paused=true — should not appear in active schedules.
        let mut paused = schedule.clone();
        paused.paused = true;
        storage.upsert_schedule(&paused).await.unwrap();

        let active = storage.get_active_schedules().await.unwrap();
        assert!(active.is_empty(), "paused schedule should not be active");

        // Delete the schedule.
        storage.delete_schedule("daily-report").await.unwrap();

        // Even if we get all (including paused), there should be none.
        let read_txn = storage.db.begin_read().unwrap();
        let table = read_txn.open_table(SCHEDULES_TABLE).unwrap();
        assert_eq!(table.len().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_remove_completed_before() {
        let storage = temp_storage();

        // Insert an old completed job.
        let mut old_job = test_job("cleanup");
        old_job.state = JobState::Completed;
        old_job.completed_at = Some(Utc::now() - Duration::days(30));
        storage.insert_job(&old_job).await.unwrap();

        // Insert a recent completed job that should NOT be removed.
        let mut recent_job = test_job("cleanup");
        recent_job.state = JobState::Completed;
        recent_job.completed_at = Some(Utc::now());
        storage.insert_job(&recent_job).await.unwrap();

        // Insert a waiting job — should not be touched.
        let waiting_job = test_job("cleanup");
        storage.insert_job(&waiting_job).await.unwrap();

        let cutoff = Utc::now() - Duration::days(7);
        let removed = storage.remove_completed_before(cutoff).await.unwrap();
        assert_eq!(removed, 1);

        // Old job is gone.
        assert!(storage.get_job(old_job.id).await.unwrap().is_none());
        // Recent completed job is still there.
        assert!(storage.get_job(recent_job.id).await.unwrap().is_some());
        // Waiting job untouched.
        assert!(storage.get_job(waiting_job.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_rebuild_indexes_on_open() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        drop(tmp);

        {
            let storage = RedbStorage::new(&path).unwrap();

            let waiting = test_job("rebuild");
            storage.insert_job(&waiting).await.unwrap();

            let mut active = test_job("rebuild");
            active.state = JobState::Active;
            storage.insert_job(&active).await.unwrap();

            let write_txn = storage.db.begin_write().unwrap();
            {
                let mut queue_index = write_txn
                    .open_table(JOBS_QUEUE_STATE_PRIORITY_INDEX)
                    .unwrap();
                let mut state_index = write_txn.open_table(JOBS_STATE_UPDATED_INDEX).unwrap();
                queue_index.retain(|_, _| false).unwrap();
                state_index.retain(|_, _| false).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let reopened = RedbStorage::new(&path).unwrap();
        let counts = reopened.get_queue_counts("rebuild").await.unwrap();
        assert_eq!(counts.waiting, 1);
        assert_eq!(counts.active, 1);
    }

    #[tokio::test]
    async fn test_none_durability_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        drop(tmp);

        let storage = RedbStorage::new_with_durability(&path, RedbDurability::None).unwrap();
        let job = test_job("none-durability");
        let id = storage.insert_job(&job).await.unwrap();
        let stored = storage
            .get_job(id)
            .await
            .unwrap()
            .expect("job should exist");
        assert_eq!(stored.id, id);
        assert_eq!(stored.queue, "none-durability");
    }
}
