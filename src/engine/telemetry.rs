//! OpenTelemetry integration for RustQueue.
//!
//! When the `otel` feature is enabled, this module provides a tracing layer
//! that exports spans to an OpenTelemetry collector via OTLP.

use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;

/// Create a tracing-opentelemetry layer configured for OTLP export.
///
/// The returned layer can be composed with other tracing subscriber layers.
/// The returned layer is generic over the subscriber type `S`, so it can be
/// composed with any subscriber that implements `tracing::Subscriber` and
/// `tracing_subscriber::registry::LookupSpan`.
///
/// # Arguments
///
/// * `service_name` - The service name to report in traces (e.g. "rustqueue")
/// * `endpoint` - The OTLP collector endpoint (e.g. "http://localhost:4317")
pub fn create_otel_layer<S>(
    service_name: &str,
    endpoint: &str,
) -> Result<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(opentelemetry_sdk::Resource::new(vec![
            KeyValue::new("service.name", service_name.to_string()),
        ]))
        .build();

    let tracer = provider.tracer("rustqueue");

    // Set the global provider so shutdown works correctly
    opentelemetry::global::set_tracer_provider(provider);

    Ok(tracing_opentelemetry::layer().with_tracer(tracer))
}

/// Shutdown the OpenTelemetry pipeline, flushing any pending spans.
pub fn shutdown_otel() {
    opentelemetry::global::shutdown_tracer_provider();
}
