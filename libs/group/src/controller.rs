use crate::crd::KanidmGroup;
use crate::reconcile::reconcile_group;

use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};

use std::sync::Arc;

use futures::StreamExt;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::watcher;
use tokio::time::Duration;
use tracing::info;

pub const CONTROLLER_ID: ControllerId = "group";

/// Initialize Kanidm controller and shared state
pub async fn run(state: State, client: Client) {
    let group = check_api_queryable::<KanidmGroup>(client.clone()).await;

    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    // https://kubernetes.io/docs/reference/using-api/api-concepts/#streaming-lists
    let group_controller = Controller::new(group, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_group),
            error_policy,
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    tokio::join!(group_controller);
}
