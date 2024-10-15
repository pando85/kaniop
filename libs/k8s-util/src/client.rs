use crate::metrics::MetricsLayer;

use hyper_util::rt::TokioExecutor;
use kube::Result;
use kube::{client::ConfigExt, Client, Config};
use prometheus_client::registry::Registry;
use tower::{BoxError, ServiceBuilder};

pub async fn new_client_with_metrics(config: Config, registry: &mut Registry) -> Result<Client> {
    let metrics_layer = MetricsLayer::new(registry);
    let https = config.rustls_https_connector()?;
    let service = ServiceBuilder::new()
        .layer(metrics_layer)
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .map_err(BoxError::from)
        .service(hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https));

    Ok(Client::new(service, config.default_namespace))
}
