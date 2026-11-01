//! One place that records every Kanidm SDK call made for a RADIUS server.

use std::future::Future;

use kaniop_operator::metrics::record_kanidm_sdk_call;
use kaniop_operator::metrics::{ControllerMetrics, KANIDM_RESOURCE_RADIUS_SERVER};

/// Run a Kanidm SDK future, recording count, duration and outcome under the
/// RADIUS resource label. `success_outcome` is one of the
/// `KANIDM_OUTCOME_*` constants.
pub(crate) async fn call<F, T, E>(
    metrics: &ControllerMetrics,
    operation: &'static str,
    success_outcome: &'static str,
    future: F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    record_kanidm_sdk_call(
        metrics,
        KANIDM_RESOURCE_RADIUS_SERVER,
        operation,
        success_outcome,
        future,
    )
    .await
}
