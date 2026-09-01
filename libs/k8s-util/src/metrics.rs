use crate::url::template_path;

use std::{
    task::{Context, Poll},
    time::Instant,
};

use http::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use tower::{Layer, Service};
use tracing::debug;
use url_escape;

const LATENCY_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];

/// Metrics layer for monitoring HTTP requests
#[derive(Clone)]
pub struct MetricsLayer {
    meter: Meter,
}

impl MetricsLayer {
    pub fn new(meter: &Meter) -> Self {
        Self {
            meter: meter.clone(),
        }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, service: S) -> Self::Service {
        MetricsService::new(service, &self.meter)
    }
}

#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    request_count: Counter<u64>,
    request_duration: Histogram<f64>,
    transport_failure_count: Counter<u64>,
    transport_failure_duration: Histogram<f64>,
}

impl<S> MetricsService<S> {
    fn new(service: S, meter: &Meter) -> Self {
        debug!("Initializing Kubernetes client metrics");
        let request_count = meter
            .u64_counter("kubernetes_client_http_requests")
            .with_description("Total number of HTTP requests")
            .build();

        let request_duration = meter
            .f64_histogram("kubernetes_client_http_request_duration_seconds")
            .with_description("HTTP request duration in seconds")
            .with_boundaries(LATENCY_BUCKETS.to_vec())
            .build();

        let transport_failure_count = meter
            .u64_counter("kubernetes_client_http_transport_failures")
            .with_description("Total number of transport-level failures")
            .build();

        let transport_failure_duration = meter
            .f64_histogram("kubernetes_client_http_transport_failure_duration_seconds")
            .with_description("Duration of failed HTTP requests in seconds")
            .with_boundaries(LATENCY_BUCKETS.to_vec())
            .build();

        debug!("Kubernetes client metrics initialized");
        Self {
            inner: service,
            request_count,
            request_duration,
            transport_failure_count,
            transport_failure_duration,
        }
    }
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for MetricsService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let path_template = template_path(req.uri().path(), None);
        let endpoint = url_escape::encode_path(&path_template).to_string();
        let start = Instant::now();

        let future = self.inner.call(req);

        MetricsFuture {
            future,
            endpoint,
            start,
            request_count: self.request_count.clone(),
            request_duration: self.request_duration.clone(),
            transport_failure_count: self.transport_failure_count.clone(),
            transport_failure_duration: self.transport_failure_duration.clone(),
        }
    }
}

#[pin_project::pin_project]
pub struct MetricsFuture<F> {
    #[pin]
    future: F,
    endpoint: String,
    start: Instant,
    request_count: Counter<u64>,
    request_duration: Histogram<f64>,
    transport_failure_count: Counter<u64>,
    transport_failure_duration: Histogram<f64>,
}

impl<F, ResBody, E> std::future::Future for MetricsFuture<F>
where
    F: std::future::Future<Output = Result<Response<ResBody>, E>>,
{
    type Output = F::Output;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let poll_result = this.future.poll(cx);

        match &poll_result {
            Poll::Ready(Ok(response)) => {
                let duration = this.start.elapsed().as_secs_f64();
                let status = response.status().as_str().to_string();

                this.request_count.add(
                    1,
                    &[
                        KeyValue::new("status", status),
                        KeyValue::new("endpoint", this.endpoint.clone()),
                    ],
                );
                this.request_duration.record(
                    duration,
                    &[KeyValue::new("endpoint", this.endpoint.clone())],
                );
            }
            Poll::Ready(Err(_)) => {
                let duration = this.start.elapsed().as_secs_f64();

                this.transport_failure_count
                    .add(1, &[KeyValue::new("endpoint", this.endpoint.clone())]);
                this.transport_failure_duration.record(
                    duration,
                    &[KeyValue::new("endpoint", this.endpoint.clone())],
                );
            }
            Poll::Pending => {}
        }

        poll_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;

    use http::{Response, StatusCode};
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use tower::service_fn;

    fn test_setup() -> (SdkMeterProvider, InMemoryMetricExporter, Meter) {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        (provider, exporter, meter)
    }

    async fn wait_for_flush() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn success_records_request_count_and_duration() {
        let (provider, exporter, meter) = test_setup();

        let svc = service_fn(|_req: http::Request<()>| async {
            Ok::<_, Infallible>(Response::builder().status(StatusCode::OK).body(()).unwrap())
        });

        let layer = MetricsLayer::new(&meter);
        let mut svc = tower::ServiceBuilder::new().layer(layer).service(svc);

        let req = http::Request::builder()
            .uri("/api/v1/pods/mypod")
            .body(())
            .unwrap();

        let _ = tower::ServiceExt::ready(&mut svc)
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap();

        provider.force_flush().unwrap();
        wait_for_flush().await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("kubernetes_client_http_requests"));
        assert!(text.contains("kubernetes_client_http_request_duration_seconds"));
    }

    #[tokio::test]
    async fn transport_error_records_failure_count_and_duration() {
        let (provider, exporter, meter) = test_setup();

        let svc = service_fn(|_req: http::Request<()>| async {
            Err::<Response<()>, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            ))
        });

        let layer = MetricsLayer::new(&meter);
        let mut svc = tower::ServiceBuilder::new().layer(layer).service(svc);

        let req = http::Request::builder()
            .uri("/api/v1/pods/mypod")
            .body(())
            .unwrap();

        let _ = tower::ServiceExt::ready(&mut svc)
            .await
            .unwrap()
            .call(req)
            .await;

        provider.force_flush().unwrap();
        wait_for_flush().await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("kubernetes_client_http_transport_failures"));
        assert!(text.contains("kubernetes_client_http_transport_failure_duration_seconds"));
    }

    #[tokio::test]
    async fn transport_error_does_not_record_success_metrics() {
        let (provider, exporter, meter) = test_setup();

        let svc = service_fn(|_req: http::Request<()>| async {
            Err::<Response<()>, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "connection refused",
            ))
        });

        let layer = MetricsLayer::new(&meter);
        let mut svc = tower::ServiceBuilder::new().layer(layer).service(svc);

        let req = http::Request::builder()
            .uri("/apis/apps/v1/namespaces/ns/deployments/dep")
            .body(())
            .unwrap();

        let _ = tower::ServiceExt::ready(&mut svc)
            .await
            .unwrap()
            .call(req)
            .await;

        provider.force_flush().unwrap();
        wait_for_flush().await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(!text.contains("kubernetes_client_http_requests"));
        assert!(!text.contains("kubernetes_client_http_request_duration_seconds"));
    }

    #[tokio::test]
    async fn error_is_passed_through_unchanged() {
        let (_provider, _exporter, meter) = test_setup();

        let svc = service_fn(|_req: http::Request<()>| async {
            Err::<Response<()>, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out",
            ))
        });

        let layer = MetricsLayer::new(&meter);
        let mut svc = tower::ServiceBuilder::new().layer(layer).service(svc);

        let req = http::Request::builder()
            .uri("/api/v1/pods")
            .body(())
            .unwrap();

        let result = tower::ServiceExt::ready(&mut svc)
            .await
            .unwrap()
            .call(req)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(err.to_string(), "timed out");
    }
}
