use crate::url::template_path;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::FutureExt;
use http::Request;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::{counter::Counter, family::Family, histogram::Histogram};
use prometheus_client::registry::Registry;
use tokio::time::Instant;
use tower::{Layer, Service};

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug, Default)]
pub struct EndpointLabel {
    pub endpoint: String,
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug, Default)]
pub struct StatusCodeLabel {
    pub status_code: String,
}

pub struct MetricsLayer {
    request_histogram: Family<EndpointLabel, Histogram>,
    requests_total: Family<StatusCodeLabel, Counter>,
}

impl MetricsLayer {
    pub fn new(registry: &mut Registry) -> Self {
        // TODO: remove bucket, implement summary (without quantiles):
        // https://github.com/prometheus/client_rust/pull/67
        let request_histogram =
            Family::<EndpointLabel, Histogram>::new_with_constructor(|| Histogram::new([]));

        let requests_total = Family::<StatusCodeLabel, Counter>::default();
        registry.register(
            "kubernetes_client_http_request_duration",
            "Summary of latencies for the Kubernetes client's requests by endpoint",
            request_histogram.clone(),
        );

        registry.register(
            "kubernetes_client_http_requests",
            "Total number of Kubernetes's client requests by status code",
            requests_total.clone(),
        );

        Self {
            request_histogram,
            requests_total,
        }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            request_histogram: self.request_histogram.clone(),
            requests_total: self.requests_total.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsService<S> {
    inner: S,
    request_histogram: Family<EndpointLabel, Histogram>,
    requests_total: Family<StatusCodeLabel, Counter>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for MetricsService<S>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let path_template = template_path(req.uri().path(), None);
        let labels = EndpointLabel {
            endpoint: url_escape::encode_path(&path_template).to_string(),
        };

        let start_time = Instant::now();

        let fut = self.inner.call(req);
        let request_histogram = self.request_histogram.clone();
        let requests_total = self.requests_total.clone();
        async move {
            let result = fut.await;
            let duration = start_time.elapsed().as_secs_f64();
            request_histogram.get_or_create(&labels).observe(duration);
            if let Ok(ref response) = result {
                let status_code = response.status().as_u16().to_string();
                requests_total
                    .get_or_create(&StatusCodeLabel { status_code })
                    .inc();
            }
            result
        }
        .boxed()
    }
}
