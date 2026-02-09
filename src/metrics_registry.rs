//! Metrics recorder installation helpers.
//!
//! RustQueue can either:
//! - install its own Prometheus recorder, or
//! - accept an externally installed/global recorder supplied by a parent system.

use anyhow::{Result, anyhow};
use metrics::Recorder;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle, PrometheusRecorder};

/// Global metrics registry configuration outcome.
#[derive(Default)]
pub struct MetricsRegistry {
    prometheus_handle: Option<PrometheusHandle>,
}

impl MetricsRegistry {
    /// Install the default Prometheus recorder unless a global recorder already exists.
    pub fn install_default_prometheus_if_unset() -> Result<Self> {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        match metrics::set_global_recorder(recorder) {
            Ok(()) => Ok(Self {
                prometheus_handle: Some(handle),
            }),
            Err(_) => Ok(Self {
                prometheus_handle: None,
            }),
        }
    }

    /// Install an externally provided recorder as the global metrics recorder.
    ///
    /// This is useful when RustQueue runs inside a larger platform with a shared
    /// metrics registry.
    pub fn install_external_recorder<R>(recorder: R) -> Result<Self>
    where
        R: Recorder + Send + Sync + 'static,
    {
        metrics::set_global_recorder(recorder)
            .map_err(|_| anyhow!("failed to set external metrics recorder"))?;
        Ok(Self {
            prometheus_handle: None,
        })
    }

    /// Install an externally created Prometheus recorder and keep its handle.
    pub fn install_external_prometheus_recorder(recorder: PrometheusRecorder) -> Result<Self> {
        let handle = recorder.handle();
        metrics::set_global_recorder(recorder)
            .map_err(|_| anyhow!("failed to set external Prometheus recorder"))?;
        Ok(Self {
            prometheus_handle: Some(handle),
        })
    }

    /// Handle used by `/api/v1/metrics/prometheus` to render metrics.
    pub fn prometheus_handle(&self) -> Option<PrometheusHandle> {
        self.prometheus_handle.clone()
    }
}
