use crate::error::{Error, Result};
use crate::metrics::MetricsLayer;

use futures::StreamExt;
use hyper_util::rt::TokioExecutor;
use kube::Result as KubeResult;
use kube::api::AttachedProcess;
use kube::{Client, Config, client::ConfigExt};
use prometheus_client::registry::Registry;
use tower::{BoxError, ServiceBuilder};

pub async fn new_client_with_metrics(
    config: Config,
    registry: &mut Registry,
) -> KubeResult<Client> {
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

pub async fn get_output(mut attached: AttachedProcess) -> Result<String> {
    let stdout = tokio_util::io::ReaderStream::new(
        attached
            .stdout()
            .ok_or_else(|| Error::MissingData("stdout".to_string()))?,
    );
    let stderr = tokio_util::io::ReaderStream::new(
        attached
            .stderr()
            .ok_or_else(|| Error::MissingData("stderr".to_string()))?,
    );

    let status_fut = attached
        .take_status()
        .ok_or_else(|| Error::MissingData("status".to_string()))?;

    let output_fut = stdout
        .filter_map(|r| async { r.ok().and_then(|v| String::from_utf8(v.to_vec()).ok()) })
        .collect::<Vec<_>>();
    let stderr_fut = stderr
        .filter_map(|r| async { r.ok().and_then(|v| String::from_utf8(v.to_vec()).ok()) })
        .collect::<Vec<_>>();

    let (out_vec, err_vec, status_opt) = tokio::join!(output_fut, stderr_fut, status_fut);
    let out = out_vec.join("");

    let status =
        status_opt.ok_or_else(|| Error::KubeExecError("process status unavailable".to_string()))?;

    match status.status {
        Some(s) if s == "Success" => Ok(out),
        _ => Err(Error::KubeExecError(format!(
            "stderr: {}: stdout: {}",
            err_vec.join("").replace("\n", "\\n"),
            out.replace("\n", "\\n"),
        ))),
    }
}
