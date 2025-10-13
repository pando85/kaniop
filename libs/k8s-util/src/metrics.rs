use std::{
    task::{Context, Poll},
    time::Instant,
};

use http::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use tower::{Layer, Service};

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
}

impl<S> MetricsService<S> {
    fn new(service: S, meter: &Meter) -> Self {
        let request_count = meter
            .u64_counter("http_requests_total")
            .with_description("Total number of HTTP requests")
            .build();

        let request_duration = meter
            .f64_histogram("http_request_duration_seconds")
            .with_description("HTTP request duration in seconds")
            .build();

        Self {
            inner: service,
            request_count,
            request_duration,
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
        let method = req.method().to_string();
        let start = Instant::now();

        let future = self.inner.call(req);

        MetricsFuture {
            future,
            method,
            start,
            request_count: self.request_count.clone(),
            request_duration: self.request_duration.clone(),
        }
    }
}

#[pin_project::pin_project]
pub struct MetricsFuture<F> {
    #[pin]
    future: F,
    method: String,
    start: Instant,
    request_count: Counter<u64>,
    request_duration: Histogram<f64>,
}

impl<F, ResBody, E> std::future::Future for MetricsFuture<F>
where
    F: std::future::Future<Output = Result<Response<ResBody>, E>>,
{
    type Output = F::Output;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let poll_result = this.future.poll(cx);

        if let Poll::Ready(Ok(response)) = &poll_result {
            let duration = this.start.elapsed().as_secs_f64();
            let status = response.status().as_str().to_string();

            this.request_count.add(
                1,
                &[
                    KeyValue::new("method", this.method.clone()),
                    KeyValue::new("status", status),
                ],
            );
            this.request_duration
                .record(duration, &[KeyValue::new("method", this.method.clone())]);
        }

        poll_result
    }
}
