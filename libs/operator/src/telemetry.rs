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
use tracing_subscriber::fmt::MakeWriter;
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
#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Newline-delimited JSON (NDJSON), with exactly one JSON object per event line.
    Json,

    /// Compact, human-readable plain-text log output.
    Text,
}

fn logger_layer<W>(
    log_format: LogFormat,
    writer: W,
) -> Box<dyn tracing_subscriber::Layer<Registry> + Send + Sync>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    match log_format {
        // `json()` already emits compact NDJSON. Calling `compact()` after `json()` would switch
        // the event formatter back to compact text while retaining JSON field formatting.
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .json()
            .boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .compact()
            .boxed(),
    }
}

/// Initializes logging and tracing subsystems.
///
/// This asynchronous function configures and initializes logging and tracing
/// according to the provided format and filtering parameters. JSON output uses
/// newline-delimited JSON (NDJSON): every tracing event is one complete JSON object
/// on one physical output line. It also supports OpenTelemetry tracing when a tracing
/// URL is specified. If OpenTelemetry is enabled, traces are sent to the given URL
/// using OTLP over HTTP.
///
/// # Example
///
/// ```rust
/// # use kaniop_operator::telemetry::{init, LogFormat};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let opentelemetry_endpoint_url = std::env::var("OPENTELEMETRY_ENDPOINT_URL").ok();
///     init("info", LogFormat::Text, opentelemetry_endpoint_url.as_deref(), 0.1)
///         .await?;
///
///     Ok(())
/// }
/// ```
///
/// # OpenTelemetry Integration
///
/// When a tracing URL is provided, OpenTelemetry tracing is configured using OTLP.
/// The function creates a tracing pipeline with a ratio-based trace sampler and a
/// default random trace ID generator. Traces will be sampled based on the
/// `trace_ratio` provided.
pub async fn init(
    log_filter: &str,
    log_format: LogFormat,
    tracing_url: Option<&str>,
    trace_ratio: f64,
) -> Result<(), Error> {
    let logger = logger_layer(log_format, std::io::stdout);

    // Safe unwrap: the static directive is valid.
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

#[cfg(test)]
mod format_tests {
    use super::*;

    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use serde_json::Value;

    #[derive(Clone, Default)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    struct TestWriterGuard(Arc<Mutex<Vec<u8>>>);

    impl TestWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl<'writer> MakeWriter<'writer> for TestWriter {
        type Writer = TestWriterGuard;

        fn make_writer(&'writer self) -> Self::Writer {
            TestWriterGuard(self.0.clone())
        }
    }

    impl Write for TestWriterGuard {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_format_is_one_object_per_line_with_canonical_message() {
        let writer = TestWriter::default();
        let subscriber = Registry::default().with(logger_layer(LogFormat::Json, writer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(controller = "test", attempts = 3_u64, "starting controller");
            tracing::warn!(error = "retryable", "controller retry scheduled");
        });

        let output = writer.contents();
        let lines = output
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 2, "each event must occupy exactly one line");

        let events = lines
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).expect("log line must be valid JSON"))
            .collect::<Vec<_>>();

        for event in &events {
            assert!(event.is_object());
            assert!(event.get("timestamp").is_some());
            assert!(event.get("level").is_some());
            assert!(event.get("target").is_some());
            assert!(event.get("fields").is_some_and(Value::is_object));
            assert!(event["fields"].get("msg").is_none());
        }

        assert_eq!(events[0]["level"], "INFO");
        assert_eq!(events[0]["fields"]["message"], "starting controller");
        assert_eq!(events[0]["fields"]["controller"], "test");
        assert_eq!(events[0]["fields"]["attempts"], 3);
    }

    #[test]
    fn text_format_remains_human_readable() {
        let writer = TestWriter::default();
        let subscriber = Registry::default().with(logger_layer(LogFormat::Text, writer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(controller = "test", "starting controller");
        });

        let output = writer.contents();
        assert!(output.contains("starting controller"));
        assert!(serde_json::from_str::<Value>(output.trim()).is_err());
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
