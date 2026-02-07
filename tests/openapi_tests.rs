//! Integration tests for the OpenAPI spec and Scalar UI endpoints.

use std::sync::Arc;

use reqwest::Client;

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::storage::MemoryStorage;

/// Spin up a test HTTP server and return the base URL.
async fn start_test_server() -> String {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let queue_manager = Arc::new(QueueManager::new(storage));

    let state = Arc::new(AppState {
        queue_manager,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: rustqueue::config::AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
    });
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn test_openapi_json_returns_valid_spec() {
    let base_url = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/api/v1/openapi.json"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.contains("json"),
        "expected JSON content-type, got: {content_type}"
    );

    let body: serde_json::Value = resp.json().await.unwrap();

    // Verify it's an OpenAPI 3.1 spec
    assert!(
        body.get("openapi").is_some(),
        "missing 'openapi' version field"
    );

    // Verify info section
    let info = body.get("info").expect("missing 'info' section");
    assert_eq!(info["title"], "RustQueue API");

    // Verify paths exist
    let paths = body.get("paths").expect("missing 'paths' section");
    assert!(
        paths.get("/api/v1/queues/{queue}/jobs").is_some(),
        "missing push/pull jobs path"
    );
    assert!(
        paths.get("/api/v1/jobs/{id}").is_some(),
        "missing get job path"
    );
    assert!(
        paths.get("/api/v1/queues").is_some(),
        "missing list queues path"
    );
    assert!(
        paths.get("/api/v1/schedules").is_some(),
        "missing schedules path"
    );
    assert!(
        paths.get("/api/v1/health").is_some(),
        "missing health path"
    );
    assert!(
        paths.get("/api/v1/events").is_some(),
        "missing websocket events path"
    );
    assert!(
        paths.get("/api/v1/metrics/prometheus").is_some(),
        "missing prometheus metrics path"
    );

    // Verify schemas exist
    let schemas = body
        .pointer("/components/schemas")
        .expect("missing schemas");
    assert!(schemas.get("Job").is_some(), "missing Job schema");
    assert!(
        schemas.get("JobState").is_some(),
        "missing JobState schema"
    );
    assert!(
        schemas.get("Schedule").is_some(),
        "missing Schedule schema"
    );
    assert!(
        schemas.get("QueueCounts").is_some(),
        "missing QueueCounts schema"
    );
}

#[tokio::test]
async fn test_scalar_docs_returns_html() {
    let base_url = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/api/v1/docs"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "expected HTML content-type, got: {content_type}"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("scalar") || body.contains("Scalar") || body.contains("api-reference"),
        "expected Scalar UI HTML"
    );
}
