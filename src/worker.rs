//! Managed worker loop for embedded `RustQueue` usage.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::warn;

use crate::builder::RustQueue;
use crate::engine::error::RustQueueError;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Keep worker error strings safely under MAX_ERROR_MESSAGE_LEN (10_240).
const MAX_WORKER_ERR: usize = 8192;

fn truncate_err(mut s: String) -> String {
    if s.len() <= MAX_WORKER_ERR {
        return s;
    }
    let mut end = MAX_WORKER_ERR;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("…(truncated)");
    s
}

/// Aborts the wrapped task when dropped. Tokio's `JoinHandle::drop` only
/// *detaches* a task — so without this guard, an early `?` return or a panic
/// in the user handler would leave the heartbeat / shutdown-watcher task
/// running. A leaked heartbeat task keeps the job's heartbeat fresh forever,
/// which would prevent stall detection from ever reclaiming the job.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RustQueue {
    /// Run a managed worker on `queue` until Ctrl-C: pulls jobs, runs `handler`,
    /// acks on `Ok`, fails (engine retries/DLQ) on `Err`. Ensures housekeeping is
    /// running. Sequential (one job at a time) — minimum-viable correctness, not a
    /// high-throughput runtime (concurrency lands in a later release).
    pub async fn run_worker<F, Fut, E>(&self, queue: &str, handler: F) -> Result<(), RustQueueError>
    where
        F: Fn(crate::Job) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: std::fmt::Display,
    {
        self.run_worker_with_shutdown(queue, handler, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Like [`run_worker`](Self::run_worker) but stops when `shutdown` resolves
    /// instead of on Ctrl-C. Use when embedding in a web server: pass the server's
    /// graceful-shutdown signal. The in-flight job is finished before returning.
    pub async fn run_worker_with_shutdown<F, Fut, E, S>(
        &self,
        queue: &str,
        handler: F,
        shutdown: S,
    ) -> Result<(), RustQueueError>
    where
        F: Fn(crate::Job) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: std::fmt::Display,
        S: Future<Output = ()> + Send + 'static,
    {
        self.start_housekeeping()?;
        let stop = Arc::new(AtomicBool::new(false));
        // Aborted on function exit (incl. early `?` return) so the watcher does
        // not outlive the worker when `shutdown` is a never-resolving future
        // such as Ctrl-C.
        let _watcher = {
            let stop = stop.clone();
            AbortOnDrop(tokio::spawn(async move {
                shutdown.await;
                stop.store(true, Ordering::SeqCst);
            }))
        };

        // Heartbeat well inside the stall window: half the timeout, clamped so we
        // neither hammer storage (floor) nor drift on long timeouts (ceiling).
        let hb_interval =
            (self.stall_timeout / 2).clamp(Duration::from_millis(200), Duration::from_secs(30));

        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let jobs = self.pull(queue, 1).await?;
            if jobs.is_empty() {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            for job in jobs {
                let job_id = job.id;
                // Heartbeat the in-flight job so stall detection does not reclaim
                // a healthy long-running job. The guard aborts the task on every
                // exit path — including a panic in the user handler.
                let _hb = {
                    let rq = self.clone();
                    AbortOnDrop(tokio::spawn(async move {
                        let mut tick = tokio::time::interval(hb_interval);
                        tick.tick().await; // consume immediate tick
                        loop {
                            tick.tick().await;
                            if rq.heartbeat(job_id).await.is_err() {
                                break;
                            }
                        }
                    }))
                };
                let outcome = handler(job).await;
                match outcome {
                    Ok(()) => {
                        if let Err(e) = self.ack(job_id, None).await {
                            warn!(job_id = %job_id, error = %e, "worker ack failed");
                        }
                    }
                    Err(err) => {
                        let msg = truncate_err(err.to_string());
                        if let Err(e) = self.fail(job_id, &msg).await {
                            warn!(job_id = %job_id, error = %e, "worker fail failed");
                        }
                    }
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
        Ok(())
    }
}
