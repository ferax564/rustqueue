use rustqueue::RustQueue;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn run_worker_acks_on_ok_and_stops_on_shutdown() {
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    let id = rq
        .push("emails", "send", json!({"to":"a@b.com"}), None)
        .await
        .unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = seen.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown(
                "emails",
                move |_job| {
                    let seen2 = seen2.clone();
                    async move {
                        seen2.fetch_add(1, Ordering::SeqCst);
                        Ok::<(), String>(())
                    }
                },
                async move {
                    let _ = rx.await;
                },
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(matches!(job.state, JobState::Completed));
}

#[tokio::test]
async fn run_worker_fails_on_err() {
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(50))
        .build()
        .unwrap();
    let id = rq.push("q", "job", json!({}), None).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown(
                "q",
                |_job| async { Err::<(), String>("nope".into()) },
                async move {
                    let _ = rx.await;
                },
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(
        matches!(job.state, JobState::Delayed | JobState::Waiting),
        "got {:?}",
        job.state
    );
}

#[tokio::test]
async fn long_handler_is_not_reclaimed_as_stalled() {
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(100))
        .stall_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let id = rq.push("slow", "job", json!({}), None).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let runs = Arc::new(AtomicUsize::new(0));
    let runs2 = runs.clone();
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown(
                "slow",
                move |_job| {
                    let runs2 = runs2.clone();
                    async move {
                        runs2.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        Ok::<(), String>(())
                    }
                },
                async move {
                    let _ = rx.await;
                },
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_secs(4)).await;
    let _ = tx.send(());
    worker.await.unwrap().unwrap();
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "job ran more than once → reclaimed mid-flight"
    );
    let job = rq.get_job(id).await.unwrap().unwrap();
    use rustqueue::JobState;
    assert!(matches!(job.state, JobState::Completed));
}

#[tokio::test]
async fn panicking_handler_does_not_leave_ghost_heartbeat() {
    // A panic in the handler must abort the heartbeat task. Otherwise a leaked
    // heartbeat keeps the job's heartbeat fresh forever and stall detection can
    // never reclaim it — the job would be stuck Active permanently.
    let rq = RustQueue::memory()
        .tick_interval(Duration::from_millis(100))
        .stall_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    rq.start_housekeeping().unwrap();
    let id = rq.push("p", "job", json!({}), None).await.unwrap();
    let worker = {
        let rq = rq.clone();
        tokio::spawn(async move {
            rq.run_worker_with_shutdown(
                "p",
                |_job| async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    Ok::<(), String>(())
                },
                std::future::pending::<()>(),
            )
            .await
        })
    };
    // The worker task unwinds on the panic; its heartbeat guard aborts the task.
    let _ = worker.await;
    // With the heartbeat stopped, stall detection reclaims the job (leaving the
    // Active state) within ~stall_timeout. Without the AbortOnDrop guard the
    // ghost heartbeat would keep it Active and this loop would time out.
    let mut reclaimed = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(j) = rq.get_job(id).await.unwrap() {
            use rustqueue::JobState;
            if !matches!(j.state, JobState::Active) {
                reclaimed = true;
                break;
            }
        }
    }
    assert!(
        reclaimed,
        "job stuck Active — ghost heartbeat prevented stall reclaim"
    );
}
