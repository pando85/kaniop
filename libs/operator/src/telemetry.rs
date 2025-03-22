use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::trace::{TraceId, TracerProvider as _};
use opentelemetry_otlp::{ExporterBuildError, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler, SdkTracerProvider};
use serde::Serialize;
use thiserror::Error;
use tracing::dispatcher::SetGlobalDefaultError;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Registry};

/// An error type representing various issues that can occur during tracing initialization.
#[derive(Error, Debug)]
pub enum Error {
    /// Error encountered when setting up OpenTelemetry tracing.
    #[error("ExporterBuildError: {0}")]
    ExporterBuildError(#[source] ExporterBuildError),

    /// Error encountered when setting the global tracing subscriber.
    #[error("SetGlobalDefaultError: {0}")]
    SetGlobalDefaultError(#[source] SetGlobalDefaultError),
}

/// Fetches the current `opentelemetry::trace::TraceId` as a hexadecimal string.
///
/// This function retrieves the `TraceId` by traversing the full tracing stack, from
/// the current [`tracing::Span`] to its corresponding [`opentelemetry::Context`].
/// It returns the trace ID associated with the current span.
///
/// # Example
///
/// ```rust
/// # use kaniop_operator::telemetry::get_trace_id;
/// let trace_id = get_trace_id();
/// println!("Current trace ID: {:?}", trace_id);
/// ```
pub fn get_trace_id() -> TraceId {
    use opentelemetry::trace::TraceContextExt as _; // opentelemetry::Context -> opentelemetry::trace::Span
    use tracing_opentelemetry::OpenTelemetrySpanExt as _; // tracing::Span to opentelemetry::Context

    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
}

/// Specifies the format of log output, either JSON or plain-text.
///
/// This enum derives `clap::ValueEnum` for use in command-line argument parsing,
/// and is serialized in lowercase when used with `serde`.
#[derive(clap::ValueEnum, Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// JSON-formatted log output.
    Json,

    /// Plain-text log output.
    Text,
}

/// Initializes logging and tracing subsystems.
///
/// This asynchronous function configures and initializes logging and tracing
/// according to the provided format and filtering parameters. It supports
/// both JSON and plain-text log formats, as well as OpenTelemetry tracing
/// when a tracing URL is specified. If OpenTelemetry is enabled, traces are
/// sent to the given URL using OTLP over gRPC.
///
/// # Example
///
/// ```rust
/// # use kaniop_operator::telemetry::{init, LogFormat};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Initialize tracing with a JSON log format and a log filter of "info".
///     let opentelemetry_endpoint_url = std::env::var("OPENTELEMETRY_ENDPOINT_URL").ok();
///     init("info", LogFormat::Text, opentelemetry_endpoint_url.as_deref(), 0.1)
///         .await?;
///
///     // Application logic here...
///
///     Ok(())
/// }
/// ```
///
/// In this example, the logging system is initialized with a plain-text format,
/// an `info` log level filter, and tracing disabled (as `None` is passed for the `some_tracing_url`).
///
/// # OpenTelemetry Integration
///
/// When a tracing URL is provided, OpenTelemetry tracing is configured using
/// OTLP (OpenTelemetry Protocol) over gRPC. The function creates a tracing pipeline
/// with a ratio-based trace sampler and a default random trace ID generator.
/// Traces will be sampled based on the `trace_ratio` provided.
///
/// The function sets a global tracing subscriber using the combination of
/// the [`tracing_subscriber`] logger and optionally the OpenTelemetry tracer if enabled.
///
/// If the tracing subsystem is successfully configured, the function returns
/// `Ok(())`, otherwise an appropriate error is returned.
pub async fn init(
    log_filter: &str,
    log_format: LogFormat,
    tracing_url: Option<&str>,
    trace_ratio: f64,
) -> Result<(), Error> {
    let logger = match log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer().json().compact().boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().compact().boxed(),
    };

    // Safe unwrap: log_filter is a valid filter directive
    let filter = EnvFilter::new(log_filter).add_directive("kanidm_client=error".parse().unwrap());

    let collector = Registry::default().with(logger).with(filter);

    if let Some(url) = tracing_url {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(url)
            .with_timeout(Duration::from_secs(3))
            .build()
            .map_err(Error::ExporterBuildError)?;

        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::TraceIdRatioBased(trace_ratio))
            .with_id_generator(RandomIdGenerator::default())
            .with_max_events_per_span(64)
            .with_max_attributes_per_span(16)
            .with_max_events_per_span(16)
            .with_resource(
                Resource::builder()
                    .with_service_name("kaniop")
                    .with_attribute(KeyValue::new("key", "value"))
                    .build(),
            )
            .with_batch_exporter(exporter)
            .build();
        let tracer = provider.tracer("opentelemetry-otlp");

        let telemetry = OpenTelemetryLayer::new(tracer);
        tracing::subscriber::set_global_default(collector.with(telemetry))
            .map_err(Error::SetGlobalDefaultError)
    } else {
        tracing::subscriber::set_global_default(collector).map_err(Error::SetGlobalDefaultError)
    }
}

#[cfg(all(test, feature = "integration-test"))]
mod test {
    // This test only works when telemetry is initialized fully
    // and requires OPENTELEMETRY_ENDPOINT_URL pointing to a valid server
    #[tokio::test]
    async fn integration_get_trace_id_returns_valid_traces() {
        use super::*;
        let opentelemetry_endpoint_url = std::env::var("OPENTELEMETRY_ENDPOINT_URL").ok();
        super::init(
            "info",
            LogFormat::Text,
            opentelemetry_endpoint_url.as_deref(),
            0.1,
        )
        .await
        .unwrap();
        #[tracing::instrument(name = "test_span")] // need to be in an instrumented fn
        fn test_trace_id() -> TraceId {
            get_trace_id()
        }
        assert_ne!(test_trace_id(), TraceId::INVALID, "valid trace");
    }
}
