use crate::crd::KanidmPersonAccount;
use crate::reconcile::reconcile_person_account;

use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{check_api_queryable, error_policy, ControllerId, State, Stores};

use futures::StreamExt;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::watcher;
use tokio::time::Duration;
use tracing::info;

pub const CONTROLLER_ID: ControllerId = "person-account";

/// Initialize Kanidm controller and shared state
pub async fn run(state: State, client: Client) {
    let kanidm = check_api_queryable::<KanidmPersonAccount>(client.clone()).await;

    let ctx = state.to_context(client, CONTROLLER_ID, Stores::default());

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    // https://kubernetes.io/docs/reference/using-api/api-concepts/#streaming-lists
    let kanidm_controller = Controller::new(kanidm, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_person_account),
            error_policy,
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    kanidm_controller.await;
}
