//! Plugin worker registry for engine-backed job processors.
//!
//! Engines register worker factories at startup. At runtime RustQueue resolves
//! the worker based on queue-level routing and/or job metadata.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;

use crate::engine::error::RustQueueError;
use crate::engine::models::Job;

const ENGINE_METADATA_KEY: &str = "engine";

/// Pluggable worker implementation that can process a pulled job.
#[async_trait]
pub trait JobProcessor: Send + Sync {
    /// Process an active job and optionally return an acknowledgement result.
    async fn process(&self, job: Job) -> Result<Option<Value>, RustQueueError>;
}

/// Factory for constructing workers for a specific engine.
pub trait WorkerFactory: Send + Sync {
    fn create(&self) -> Arc<dyn JobProcessor>;
}

/// Registry for engine worker factories and queue routing rules.
#[derive(Default)]
pub struct WorkerRegistry {
    factories: DashMap<String, Arc<dyn WorkerFactory>>,
    queue_to_engine: DashMap<String, String>,
}

impl WorkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a worker factory for an engine identifier.
    pub fn register_engine_factory(
        &self,
        engine: impl Into<String>,
        factory: Arc<dyn WorkerFactory>,
    ) {
        self.factories
            .insert(engine.into().to_ascii_lowercase(), factory);
    }

    /// Route a queue to a default engine.
    pub fn route_queue_to_engine(
        &self,
        queue: impl Into<String>,
        engine: impl Into<String>,
    ) -> Result<(), RustQueueError> {
        let queue = queue.into();
        let engine = engine.into().to_ascii_lowercase();
        if !self.factories.contains_key(&engine) {
            return Err(RustQueueError::ValidationError(format!(
                "No worker factory registered for engine '{engine}'"
            )));
        }
        self.queue_to_engine.insert(queue, engine);
        Ok(())
    }

    /// Resolve the engine for a job based on metadata first, then queue route.
    pub fn resolve_engine(&self, queue: &str, metadata: Option<&Value>) -> Option<String> {
        if let Some(engine) = metadata_engine(metadata) {
            return Some(engine);
        }
        self.queue_to_engine
            .get(queue)
            .map(|entry| entry.value().clone())
    }

    /// Resolve and construct a worker for the given queue/metadata.
    ///
    /// Returns `Ok(None)` when no route matches.
    pub fn resolve_worker(
        &self,
        queue: &str,
        metadata: Option<&Value>,
    ) -> Result<Option<Arc<dyn JobProcessor>>, RustQueueError> {
        let Some(engine) = self.resolve_engine(queue, metadata) else {
            return Ok(None);
        };

        let Some(factory) = self.factories.get(&engine) else {
            return Err(RustQueueError::ValidationError(format!(
                "No worker factory registered for engine '{engine}'"
            )));
        };

        Ok(Some(factory.value().create()))
    }
}

fn metadata_engine(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|meta| meta.get(ENGINE_METADATA_KEY))
        .and_then(Value::as_str)
        .map(|engine| engine.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoFactory {
        engine: &'static str,
    }

    impl WorkerFactory for EchoFactory {
        fn create(&self) -> Arc<dyn JobProcessor> {
            Arc::new(EchoProcessor {
                engine: self.engine.to_string(),
            })
        }
    }

    struct EchoProcessor {
        engine: String,
    }

    #[async_trait]
    impl JobProcessor for EchoProcessor {
        async fn process(&self, _job: Job) -> Result<Option<Value>, RustQueueError> {
            Ok(Some(serde_json::json!({"engine": self.engine})))
        }
    }

    #[test]
    fn queue_route_resolves_registered_engine() {
        let registry = WorkerRegistry::new();
        registry.register_engine_factory(
            "pixelforge",
            Arc::new(EchoFactory {
                engine: "pixelforge",
            }),
        );
        registry
            .route_queue_to_engine("image-process", "pixelforge")
            .unwrap();

        let resolved = registry.resolve_engine("image-process", None);
        assert_eq!(resolved.as_deref(), Some("pixelforge"));
    }

    #[test]
    fn metadata_engine_overrides_queue_route() {
        let registry = WorkerRegistry::new();
        registry.register_engine_factory(
            "pixelforge",
            Arc::new(EchoFactory {
                engine: "pixelforge",
            }),
        );
        registry.register_engine_factory("ferrox", Arc::new(EchoFactory { engine: "ferrox" }));
        registry
            .route_queue_to_engine("image-process", "pixelforge")
            .unwrap();

        let metadata = serde_json::json!({"engine": "ferrox"});
        let resolved = registry.resolve_engine("image-process", Some(&metadata));
        assert_eq!(resolved.as_deref(), Some("ferrox"));
    }

    #[test]
    fn unknown_engine_route_is_rejected() {
        let registry = WorkerRegistry::new();
        let err = registry
            .route_queue_to_engine("image-process", "unknown-engine")
            .unwrap_err();
        match err {
            RustQueueError::ValidationError(msg) => {
                assert!(msg.contains("No worker factory registered"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }
}
