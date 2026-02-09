//! Integration tests for workflow primitives and plugin worker routing.

use std::sync::Arc;

use async_trait::async_trait;
use rustqueue::{
    Job, JobOptions, JobProcessor, JobState, RustQueue, RustQueueError, WorkerFactory,
    WorkerRegistry, Workflow, WorkflowStep,
};
use serde_json::{Value, json};

struct AddFieldStep {
    engine: &'static str,
    key: &'static str,
    value: &'static str,
}

#[async_trait]
impl WorkflowStep for AddFieldStep {
    fn engine(&self) -> &str {
        self.engine
    }

    async fn execute(&self, mut input: Value) -> anyhow::Result<Value> {
        if let Value::Object(ref mut map) = input {
            map.insert(self.key.to_string(), json!(self.value));
            return Ok(input);
        }
        anyhow::bail!("expected object");
    }
}

#[tokio::test]
async fn workflow_runs_sequential_steps() {
    let workflow = Workflow::new()
        .then(Arc::new(AddFieldStep {
            engine: "ferrox",
            key: "extract",
            value: "ok",
        }))
        .then(Arc::new(AddFieldStep {
            engine: "pixelforge",
            key: "render",
            value: "done",
        }));

    let output = workflow.execute(json!({})).await.unwrap();
    assert_eq!(output["extract"], "ok");
    assert_eq!(output["render"], "done");
}

struct EngineFactory {
    engine: &'static str,
}

impl WorkerFactory for EngineFactory {
    fn create(&self) -> Arc<dyn JobProcessor> {
        Arc::new(EngineProcessor {
            engine: self.engine.to_string(),
        })
    }
}

struct EngineProcessor {
    engine: String,
}

#[async_trait]
impl JobProcessor for EngineProcessor {
    async fn process(&self, job: Job) -> Result<Option<Value>, RustQueueError> {
        Ok(Some(json!({
            "engine": self.engine,
            "job": job.name,
        })))
    }
}

#[tokio::test]
async fn plugin_registry_routes_by_queue_and_metadata() {
    let registry = Arc::new(WorkerRegistry::new());
    registry.register_engine_factory(
        "pixelforge",
        Arc::new(EngineFactory {
            engine: "pixelforge",
        }),
    );
    registry.register_engine_factory("ferrox", Arc::new(EngineFactory { engine: "ferrox" }));
    registry
        .route_queue_to_engine("image-process", "pixelforge")
        .unwrap();

    let rq = RustQueue::memory()
        .with_worker_registry(Arc::clone(&registry))
        .build()
        .unwrap();

    let id_default = rq
        .push("image-process", "resize", json!({"asset": "a.png"}), None)
        .await
        .unwrap();
    let processed_default = rq
        .dispatch_next_with_registered_worker("image-process")
        .await
        .unwrap();
    assert_eq!(processed_default, Some(id_default));
    let default_job = rq.get_job(id_default).await.unwrap().unwrap();
    assert_eq!(default_job.state, JobState::Completed);
    assert_eq!(default_job.result.unwrap()["engine"], "pixelforge");

    let override_opts = JobOptions {
        metadata: Some(json!({"engine": "ferrox"})),
        ..Default::default()
    };
    let id_override = rq
        .push(
            "image-process",
            "pdf-render",
            json!({"asset": "doc.pdf"}),
            Some(override_opts),
        )
        .await
        .unwrap();
    let processed_override = rq
        .dispatch_next_with_registered_worker("image-process")
        .await
        .unwrap();
    assert_eq!(processed_override, Some(id_override));
    let override_job = rq.get_job(id_override).await.unwrap().unwrap();
    assert_eq!(override_job.result.unwrap()["engine"], "ferrox");
}
