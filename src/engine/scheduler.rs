//! Background scheduler that runs periodic housekeeping tasks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::config::RetentionConfig;
use crate::engine::metrics as metric_names;
use crate::engine::queue::QueueManager;

/// Start the background scheduler that periodically runs housekeeping tasks.
///
/// This spawns a tokio task that runs on a configurable interval:
/// - Promote delayed jobs whose delay has expired
/// - Execute due schedules (cron + interval)
/// - Check for timed-out jobs
/// - Detect stalled jobs (no heartbeat within `stall_timeout_ms`)
/// - Update queue depth gauge metrics
/// - Run retention cleanup every 60 ticks (~1 minute at 1s tick)
///
/// Returns a `JoinHandle` that can be used to abort the scheduler.
pub fn start_scheduler(
    manager: Arc<QueueManager>,
    tick_interval_ms: u64,
    stall_timeout_ms: u64,
    retention: RetentionConfig,
) -> tokio::task::JoinHandle<()> {
    let interval = Duration::from_millis(tick_interval_ms);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the first immediate tick
        ticker.tick().await;

        info!(
            interval_ms = tick_interval_ms,
            stall_timeout_ms, "Background scheduler started"
        );

        let mut tick_count: u64 = 0;

        loop {
            ticker.tick().await;
            tick_count += 1;
            let tick_start = Instant::now();

            // Promote delayed jobs first
            if let Err(e) = manager.promote_delayed_jobs().await {
                warn!(error = %e, "Delayed job promotion failed");
            }

            // Execute due schedules
            if let Err(e) = manager.execute_schedules().await {
                warn!(error = %e, "Schedule execution failed");
            }

            // Check for timed-out jobs
            if let Err(e) = manager.check_timeouts().await {
                warn!(error = %e, "Timeout check failed");
            }

            // Detect stalled jobs (no heartbeat)
            if let Err(e) = manager.detect_stalls(stall_timeout_ms).await {
                warn!(error = %e, "Stall detection failed");
            }

            // Update queue depth gauges every tick
            if let Ok(queues) = manager.list_queues().await {
                for qi in &queues {
                    let q = qi.name.as_str();
                    metrics::gauge!(metric_names::QUEUE_WAITING_JOBS, "queue" => q.to_string())
                        .set(qi.counts.waiting as f64);
                    metrics::gauge!(metric_names::QUEUE_ACTIVE_JOBS, "queue" => q.to_string())
                        .set(qi.counts.active as f64);
                    metrics::gauge!(metric_names::QUEUE_DELAYED_JOBS, "queue" => q.to_string())
                        .set(qi.counts.delayed as f64);
                    metrics::gauge!(metric_names::QUEUE_DLQ_JOBS, "queue" => q.to_string())
                        .set(qi.counts.dlq as f64);
                }
            }

            // Retention cleanup every 60 ticks (~1 minute at 1s tick)
            if tick_count % 60 == 0 {
                if let Err(e) = manager
                    .cleanup_expired_jobs(
                        &retention.completed_ttl,
                        &retention.failed_ttl,
                        &retention.dlq_ttl,
                    )
                    .await
                {
                    warn!(error = %e, "Retention cleanup failed");
                }
            }

            // Record tick duration
            metrics::gauge!(metric_names::SCHEDULER_TICK_DURATION_SECONDS)
                .set(tick_start.elapsed().as_secs_f64());
        }
    })
}
