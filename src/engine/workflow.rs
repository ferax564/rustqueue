//! Workflow execution primitives for multi-engine orchestration.
//!
//! A workflow is an ordered sequence of stages. Stages can be:
//! - Sequential: one step transforms the current payload.
//! - Fan-out + fan-in: multiple steps run in parallel on the same input, then a
//!   join strategy combines their outputs into a new payload.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::Value;

/// A unit of workflow work dispatched to an engine.
#[async_trait]
pub trait WorkflowStep: Send + Sync {
    /// Engine identifier (for example: `"pixelforge"`, `"oxid"`, `"ferrox"`).
    fn engine(&self) -> &str;

    /// Execute the step and return its output payload.
    async fn execute(&self, input: Value) -> Result<Value>;
}

/// Strategy used to combine fan-out outputs into a single payload.
#[async_trait]
pub trait WorkflowJoin: Send + Sync {
    async fn join(&self, outputs: Vec<Value>) -> Result<Value>;
}

/// Default join strategy: collect fan-out outputs into a JSON array.
#[derive(Debug, Default)]
pub struct CollectArrayJoin;

#[async_trait]
impl WorkflowJoin for CollectArrayJoin {
    async fn join(&self, outputs: Vec<Value>) -> Result<Value> {
        Ok(Value::Array(outputs))
    }
}

type SharedStep = Arc<dyn WorkflowStep>;
type SharedJoin = Arc<dyn WorkflowJoin>;

enum WorkflowStage {
    Sequential(SharedStep),
    Parallel {
        steps: Vec<SharedStep>,
        join: SharedJoin,
    },
}

/// Ordered workflow with sequential and fan-out/fan-in stages.
#[derive(Default)]
pub struct Workflow {
    stages: Vec<WorkflowStage>,
}

impl Workflow {
    /// Create an empty workflow.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a sequential step.
    pub fn then(mut self, step: SharedStep) -> Self {
        self.stages.push(WorkflowStage::Sequential(step));
        self
    }

    /// Append a parallel fan-out stage with default array join.
    pub fn fan_out(mut self, steps: Vec<SharedStep>) -> Self {
        self.stages.push(WorkflowStage::Parallel {
            steps,
            join: Arc::new(CollectArrayJoin),
        });
        self
    }

    /// Append a parallel fan-out stage with a custom fan-in join strategy.
    pub fn fan_out_with_join(mut self, steps: Vec<SharedStep>, join: SharedJoin) -> Self {
        self.stages.push(WorkflowStage::Parallel { steps, join });
        self
    }

    /// Execute the workflow from the given initial input.
    pub async fn execute(&self, mut input: Value) -> Result<Value> {
        for stage in &self.stages {
            match stage {
                WorkflowStage::Sequential(step) => {
                    input = step.execute(input).await?;
                }
                WorkflowStage::Parallel { steps, join } => {
                    if steps.is_empty() {
                        bail!("fan-out stage must contain at least one step");
                    }

                    let mut handles = Vec::with_capacity(steps.len());
                    for step in steps {
                        let step = Arc::clone(step);
                        let step_input = input.clone();
                        handles.push(tokio::spawn(async move { step.execute(step_input).await }));
                    }

                    let mut outputs = Vec::with_capacity(handles.len());
                    for handle in handles {
                        let output = handle
                            .await
                            .map_err(|e| anyhow!("workflow fan-out task failed: {e}"))??;
                        outputs.push(output);
                    }

                    input = join.join(outputs).await?;
                }
            }
        }

        Ok(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        async fn execute(&self, mut input: Value) -> Result<Value> {
            if let Value::Object(ref mut map) = input {
                map.insert(self.key.to_string(), Value::String(self.value.to_string()));
                return Ok(input);
            }
            bail!("expected object input")
        }
    }

    struct AddNumberStep {
        engine: &'static str,
        amount: i64,
    }

    #[async_trait]
    impl WorkflowStep for AddNumberStep {
        fn engine(&self) -> &str {
            self.engine
        }

        async fn execute(&self, input: Value) -> Result<Value> {
            let n = input
                .as_i64()
                .ok_or_else(|| anyhow!("expected integer payload"))?;
            Ok(Value::Number((n + self.amount).into()))
        }
    }

    struct SumJoin;

    #[async_trait]
    impl WorkflowJoin for SumJoin {
        async fn join(&self, outputs: Vec<Value>) -> Result<Value> {
            let mut sum = 0i64;
            for v in outputs {
                sum += v
                    .as_i64()
                    .ok_or_else(|| anyhow!("expected integer output"))?;
            }
            Ok(Value::Number(sum.into()))
        }
    }

    #[tokio::test]
    async fn sequential_steps_pipe_data() {
        let wf = Workflow::new()
            .then(Arc::new(AddFieldStep {
                engine: "ferrox",
                key: "step1",
                value: "ok",
            }))
            .then(Arc::new(AddFieldStep {
                engine: "pixelforge",
                key: "step2",
                value: "done",
            }));

        let out = wf.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(out["step1"], "ok");
        assert_eq!(out["step2"], "done");
    }

    #[tokio::test]
    async fn fan_out_and_fan_in_with_custom_join() {
        let wf = Workflow::new().fan_out_with_join(
            vec![
                Arc::new(AddNumberStep {
                    engine: "oxid",
                    amount: 1,
                }),
                Arc::new(AddNumberStep {
                    engine: "ferrox",
                    amount: 5,
                }),
            ],
            Arc::new(SumJoin),
        );

        let out = wf.execute(serde_json::json!(10)).await.unwrap();
        assert_eq!(out, serde_json::json!(26));
    }

    #[tokio::test]
    async fn empty_fan_out_stage_is_invalid() {
        let wf = Workflow::new().fan_out(vec![]);
        let err = wf.execute(serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("at least one step"));
    }
}
