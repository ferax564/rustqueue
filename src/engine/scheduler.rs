//! Background scheduler that runs periodic housekeeping tasks.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::engine::queue::QueueManager;

/// Start the background scheduler that periodically runs housekeeping tasks.
///
/// This spawns a tokio task that runs on a configurable interval:
/// - Check for timed-out jobs
/// - Detect stalled jobs (no heartbeat within `stall_timeout_ms`)
///
/// Returns a `JoinHandle` that can be used to abort the scheduler.
pub fn start_scheduler(
    manager: Arc<QueueManager>,
    tick_interval_ms: u64,
    stall_timeout_ms: u64,
) -> tokio::task::JoinHandle<()> {
    let interval = Duration::from_millis(tick_interval_ms);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the first immediate tick
        ticker.tick().await;

        info!(interval_ms = tick_interval_ms, stall_timeout_ms, "Background scheduler started");

        loop {
            ticker.tick().await;

            // Promote delayed jobs first
            if let Err(e) = manager.promote_delayed_jobs().await {
                warn!(error = %e, "Delayed job promotion failed");
            }

            // Check for timed-out jobs
            if let Err(e) = manager.check_timeouts().await {
                warn!(error = %e, "Timeout check failed");
            }

            // Detect stalled jobs (no heartbeat)
            if let Err(e) = manager.detect_stalls(stall_timeout_ms).await {
                warn!(error = %e, "Stall detection failed");
            }
        }
    })
}
