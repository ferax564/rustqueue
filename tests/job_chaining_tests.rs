//! Integration tests for metadata-driven cross-queue follow-up chaining.

use rustqueue::{JobOptions, RustQueue};
use serde_json::json;

fn rq() -> RustQueue {
    RustQueue::memory().build().unwrap()
}

#[tokio::test]
async fn ack_enqueues_follow_up_job_with_flow_and_dependency() {
    let rq = rq();

    let options = JobOptions {
        flow_id: Some("forgepipe-flow-1".to_string()),
        metadata: Some(json!({
            "workflow_id": "forgepipe-flow-1",
            "follow_ups": [
                {
                    "queue": "image-process",
                    "name": "render-pages",
                    "data": {"doc_id": "d-1"}
                }
            ]
        })),
        ..Default::default()
    };

    let parent_id = rq
        .push(
            "document-extract",
            "extract-text",
            json!({"doc_id": "d-1"}),
            Some(options),
        )
        .await
        .unwrap();

    rq.pull("document-extract", 1).await.unwrap();
    rq.ack(parent_id, Some(json!({"pages": 3}))).await.unwrap();

    let children = rq.pull("image-process", 1).await.unwrap();
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.name, "render-pages");
    assert_eq!(child.flow_id.as_deref(), Some("forgepipe-flow-1"));
    assert_eq!(child.depends_on, vec![parent_id]);
    assert_eq!(
        child.metadata.as_ref().unwrap()["workflow_id"],
        json!("forgepipe-flow-1")
    );
    assert!(child.metadata.as_ref().unwrap().get("follow_ups").is_none());
}

#[tokio::test]
async fn follow_up_uses_parent_result_when_data_is_omitted() {
    let rq = rq();
    let options = JobOptions {
        metadata: Some(json!({
            "follow_ups": [
                {
                    "queue": "ai-inference",
                    "name": "infer"
                }
            ]
        })),
        ..Default::default()
    };

    let parent_id = rq
        .push(
            "image-process",
            "upscale",
            json!({"asset": "img-1"}),
            Some(options),
        )
        .await
        .unwrap();

    rq.pull("image-process", 1).await.unwrap();
    rq.ack(parent_id, Some(json!({"artifact": "s3://bucket/out.png"})))
        .await
        .unwrap();

    let children = rq.pull("ai-inference", 1).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].data, json!({"artifact": "s3://bucket/out.png"}));
}

#[tokio::test]
async fn malformed_follow_up_metadata_does_not_break_ack() {
    let rq = rq();
    let options = JobOptions {
        metadata: Some(json!({
            "follow_ups": {"queue": "bad-shape"}
        })),
        ..Default::default()
    };

    let parent_id = rq
        .push("work", "parent", json!({}), Some(options))
        .await
        .unwrap();

    rq.pull("work", 1).await.unwrap();
    rq.ack(parent_id, None).await.unwrap();

    let child_jobs = rq.pull("bad-shape", 10).await.unwrap();
    assert!(
        child_jobs.is_empty(),
        "malformed follow-up metadata should be ignored"
    );
}
