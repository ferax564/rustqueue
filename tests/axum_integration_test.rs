//! Integration tests for the Axum extractor.

use axum::routing::{get, post};
use axum::{Json, Router};
use rustqueue::RustQueue;
use rustqueue::axum_integration::RqState;
use serde_json::json;
use std::sync::Arc;

async fn push_handler(rq: RqState, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let name = body["name"].as_str().unwrap_or("task").to_owned();
    let id = rq.push("default", &name, body, None).await.unwrap();
    Json(json!({"id": id.to_string()}))
}

async fn stats_handler(rq: RqState) -> Json<serde_json::Value> {
    let queues = rq.list_queues().await.unwrap();
    Json(json!({"queues": queues.len()}))
}

fn app(rq: RustQueue) -> Router {
    Router::new()
        .route("/push", post(push_handler))
        .route("/stats", get(stats_handler))
        .with_state(Arc::new(rq))
}

#[tokio::test]
async fn test_rqstate_extractor_push_and_stats() {
    let rq = RustQueue::memory().build().unwrap();
    let app = app(rq);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    // Push a job
    let resp = client
        .post(format!("http://{addr}/push"))
        .json(&json!({"name": "test-job", "data": 42}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["id"].as_str().is_some());

    // Check stats
    let resp = client
        .get(format!("http://{addr}/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["queues"].as_u64().unwrap(), 1);
}
