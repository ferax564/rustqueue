//! Production smoke test exercising the full Phase 3 feature set working together:
//! authentication, job lifecycle, dashboard, and CORS.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{json, Value};

use rustqueue::api::{self, AppState};
use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Start a test server with auth ENABLED and return the base URL.
async fn start_auth_server() -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let qm = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager: qm,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: AuthConfig {
            enabled: true,
            tokens: vec!["test-token".to_string()],
        },
    });
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// End-to-end production scenario covering auth, job lifecycle, dashboard, and CORS.
#[tokio::test]
async fn test_production_scenario() {
    let base = start_auth_server().await;
    let client = Client::new();

    // ── Step 1: Health check is public (no auth needed) ──────────────────

    let health_resp = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(health_resp.status(), 200, "health endpoint should be public");
    let health_body: Value = health_resp.json().await.unwrap();
    assert_eq!(health_body["ok"], true);
    assert_eq!(health_body["status"], "healthy");

    // ── Step 2: Protected endpoint rejects unauthenticated request ───────

    let unauth_resp = client
        .post(format!("{base}/api/v1/queues/production/jobs"))
        .json(&json!({"name": "task", "data": {}}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        unauth_resp.status(),
        401,
        "protected endpoint should reject unauthenticated requests"
    );
    let unauth_body: Value = unauth_resp.json().await.unwrap();
    assert_eq!(unauth_body["ok"], false);
    assert_eq!(unauth_body["error"]["code"], "UNAUTHORIZED");

    // ── Step 3: Push a job with valid auth token ─────────────────────────

    let push_resp = client
        .post(format!("{base}/api/v1/queues/production/jobs"))
        .header("Authorization", "Bearer test-token")
        .json(&json!({
            "name": "send-email",
            "data": {"to": "user@example.com", "subject": "Welcome"}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(push_resp.status(), 201, "push with valid token should return 201");
    let push_body: Value = push_resp.json().await.unwrap();
    assert_eq!(push_body["ok"], true);
    let job_id = push_body["id"].as_str().expect("response should contain a job id");
    uuid::Uuid::parse_str(job_id).expect("id should be a valid UUID");

    // ── Step 4: Pull the job with valid auth token ───────────────────────

    let pull_resp = client
        .get(format!("{base}/api/v1/queues/production/jobs"))
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(pull_resp.status(), 200, "pull with valid token should succeed");
    let pull_body: Value = pull_resp.json().await.unwrap();
    assert_eq!(pull_body["ok"], true);
    assert!(pull_body["job"].is_object(), "pull should return a job object");
    assert_eq!(
        pull_body["job"]["id"], job_id,
        "pulled job should match pushed job id"
    );
    assert_eq!(
        pull_body["job"]["state"], "active",
        "pulled job should be in active state"
    );

    // ── Step 5: Ack the job ──────────────────────────────────────────────

    let ack_resp = client
        .post(format!("{base}/api/v1/jobs/{job_id}/ack"))
        .header("Authorization", "Bearer test-token")
        .json(&json!({"result": null}))
        .send()
        .await
        .unwrap();

    assert_eq!(ack_resp.status(), 200, "ack should return 200 OK");
    let ack_body: Value = ack_resp.json().await.unwrap();
    assert_eq!(ack_body["ok"], true);

    // ── Step 6: Verify dashboard is accessible at GET /dashboard ─────────
    // Dashboard is protected when auth is enabled, so we must pass the token.

    let dashboard_resp = client
        .get(format!("{base}/dashboard"))
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(
        dashboard_resp.status(),
        200,
        "dashboard should be accessible with valid auth token"
    );
    let content_type = dashboard_resp
        .headers()
        .get("content-type")
        .expect("dashboard should have content-type header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "dashboard should serve HTML, got: {content_type}"
    );
    let dashboard_body = dashboard_resp.text().await.unwrap();
    assert!(
        dashboard_body.contains("RustQueue"),
        "dashboard HTML should contain 'RustQueue'"
    );

    // ── Step 7: Verify CORS preflight works ──────────────────────────────

    let cors_resp = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/api/v1/health"),
        )
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "GET")
        .header("Access-Control-Request-Headers", "Authorization")
        .send()
        .await
        .unwrap();

    let acao = cors_resp
        .headers()
        .get("access-control-allow-origin")
        .expect("CORS preflight should return Access-Control-Allow-Origin");
    assert_eq!(acao.to_str().unwrap(), "*");

    assert!(
        cors_resp
            .headers()
            .get("access-control-allow-methods")
            .is_some(),
        "CORS preflight should return Access-Control-Allow-Methods"
    );

    assert!(
        cors_resp
            .headers()
            .get("access-control-allow-headers")
            .is_some(),
        "CORS preflight should return Access-Control-Allow-Headers"
    );

    // ── Step 8: Verify queue stats reflect completed job ─────────────────

    let stats_resp = client
        .get(format!("{base}/api/v1/queues/production/stats"))
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(stats_resp.status(), 200);
    let stats_body: Value = stats_resp.json().await.unwrap();
    assert_eq!(stats_body["ok"], true);
    assert_eq!(
        stats_body["counts"]["waiting"], 0,
        "no jobs should be waiting after ack"
    );
    assert_eq!(
        stats_body["counts"]["completed"], 1,
        "one job should be completed"
    );
}
