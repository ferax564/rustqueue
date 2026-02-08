//! Integration tests for the webhook system.

use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value, json};

use rustqueue::api::{self, AppState};
use rustqueue::engine::queue::QueueManager;
use rustqueue::engine::webhook::{WebhookConfig, WebhookManager};
use rustqueue::storage::MemoryStorage;

/// Start a test server with webhooks enabled and return (base_url, webhook_manager).
async fn start_webhook_server() -> (String, Arc<WebhookManager>) {
    let (event_tx, _) = tokio::sync::broadcast::channel(1024);
    let storage = Arc::new(MemoryStorage::new());
    let qm = Arc::new(QueueManager::new(storage).with_event_sender(event_tx.clone()));

    let webhook_config = WebhookConfig {
        enabled: true,
        delivery_timeout_ms: 2000,
        max_retries: 1,
        retry_base_delay_ms: 100,
    };
    let webhook_manager = Arc::new(WebhookManager::new(webhook_config));
    let _dispatcher = rustqueue::engine::webhook::start_webhook_dispatcher(
        Arc::clone(&webhook_manager),
        event_tx.subscribe(),
    );

    let state = Arc::new(AppState {
        queue_manager: qm,
        start_time: std::time::Instant::now(),
        metrics_handle: None,
        event_tx,
        auth_config: rustqueue::config::AuthConfig::default(),
        auth_rate_limiter: rustqueue::api::auth::AuthRateLimiter::new(),
        webhook_manager: Some(Arc::clone(&webhook_manager)),
    });
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{addr}"), webhook_manager)
}

#[tokio::test]
async fn test_webhook_crud_via_api() {
    let (base, _mgr) = start_webhook_server().await;
    let client = Client::new();

    // Register a webhook
    let resp: Value = client
        .post(format!("{base}/api/v1/webhooks"))
        .json(&json!({
            "url": "http://example.com/hook",
            "events": ["job_completed", "job_failed"],
            "queues": ["emails"],
            "secret": "my-secret-123"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["ok"], true);
    let webhook_id = resp["webhook"]["id"].as_str().unwrap().to_string();
    assert_eq!(resp["webhook"]["url"], "http://example.com/hook");
    // Secret should NOT be serialized in responses
    assert!(resp["webhook"]["secret"].is_null());

    // List webhooks
    let resp: Value = client
        .get(format!("{base}/api/v1/webhooks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["ok"], true);
    assert_eq!(resp["webhooks"].as_array().unwrap().len(), 1);

    // Get single webhook
    let resp: Value = client
        .get(format!("{base}/api/v1/webhooks/{webhook_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["ok"], true);
    assert_eq!(resp["webhook"]["url"], "http://example.com/hook");

    // Delete webhook
    let resp: Value = client
        .delete(format!("{base}/api/v1/webhooks/{webhook_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["ok"], true);

    // Verify deleted
    let status = client
        .get(format!("{base}/api/v1/webhooks/{webhook_id}"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status, 404);
}

#[tokio::test]
async fn test_webhook_event_filtering() {
    let (_base, mgr) = start_webhook_server().await;

    // Register webhook filtered to only job_completed on "emails" queue
    let input = rustqueue::engine::webhook::WebhookInput {
        url: "http://example.com/hook".into(),
        events: vec![rustqueue::engine::webhook::WebhookEventType::JobCompleted],
        queues: vec!["emails".into()],
        secret: None,
    };
    let wh = mgr.register(input);

    // This webhook should be listed
    assert_eq!(mgr.list().len(), 1);
    assert_eq!(mgr.get(wh.id).unwrap().active, true);
}

#[tokio::test]
async fn test_webhook_hmac_signing() {
    // Verify HMAC-SHA256 signature is deterministic and correct length
    let sig1 = rustqueue::engine::webhook::WebhookManager::sign_payload("secret", b"payload");
    let sig2 = rustqueue::engine::webhook::WebhookManager::sign_payload("secret", b"payload");
    assert_eq!(sig1, sig2);
    assert_eq!(sig1.len(), 64); // 32 bytes hex-encoded

    // Different secret → different signature
    let sig3 = rustqueue::engine::webhook::WebhookManager::sign_payload("other", b"payload");
    assert_ne!(sig1, sig3);
}

#[tokio::test]
async fn test_webhook_not_found() {
    let (base, _mgr) = start_webhook_server().await;
    let client = Client::new();

    let status = client
        .get(format!(
            "{base}/api/v1/webhooks/00000000-0000-0000-0000-000000000000"
        ))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status, 404);
}

#[tokio::test]
async fn test_webhook_validation_empty_url() {
    let (base, _mgr) = start_webhook_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/webhooks"))
        .json(&json!({"url": "", "events": []}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}
