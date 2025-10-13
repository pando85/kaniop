use crate::metrics::MetricsLayer;

use futures::StreamExt;
use hyper_util::rt::TokioExecutor;
use kube::Result;
use kube::api::AttachedProcess;
use kube::{Client, Config, client::ConfigExt};
use opentelemetry::metrics::Meter;
use tower::{BoxError, ServiceBuilder};

pub async fn new_client_with_metrics(config: Config, meter: &Meter) -> Result<Client> {
    let metrics_layer = MetricsLayer::new(meter);
    let https = config.rustls_https_connector()?;
    let service = ServiceBuilder::new()
        .layer(metrics_layer)
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .map_err(BoxError::from)
        .service(hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https));

    Ok(Client::new(service, config.default_namespace))
}

pub async fn get_output(mut attached: AttachedProcess) -> Option<String> {
    let stdout = tokio_util::io::ReaderStream::new(attached.stdout()?);
    let out = stdout
        .filter_map(|r| async { r.ok().and_then(|v| String::from_utf8(v.to_vec()).ok()) })
        .collect::<Vec<_>>()
        .await
        .join("");
    attached.join().await.ok()?;
    Some(out)
}
