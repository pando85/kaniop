use crate::controller::{Context, State};
use crate::crd::echo::Echo;
use crate::echo::reconcile::reconcile_echo;
use crate::error::Error;

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{Api, ListParams, ResourceExt};
use kube::client::Client;
use kube::runtime::controller::{self, Action, Controller};
use kube::runtime::reflector::{self, ReflectHandle};
use kube::runtime::{watcher, WatchStreamExt};
use tokio::time::Duration;
use tracing::{debug, error, info};

const SUBSCRIBE_BUFFER_SIZE: usize = 256;
const RELOAD_BUFFER_SIZE: usize = 16;

fn error_policy<K: ResourceExt>(
    obj: Arc<K>,
    error: &Error,
    ctx: Arc<Context<Deployment>>,
) -> Action {
    // safe unwrap: deployment is a namespace scoped resource
    error!(msg = "failed reconciliation", namespace = %obj.namespace().unwrap(), name = %obj.name_any(), %error);
    ctx.metrics.reconcile.set_failure(&obj, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

/// Initialize echoes controller and shared state (given the crd is installed)
pub async fn run(state: State) {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let echo = Api::<Echo>::all(client.clone());
    if let Err(e) = echo.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        std::process::exit(1);
    }

    let (deployment_store, writer) = reflector::store_shared(SUBSCRIBE_BUFFER_SIZE);
    let subscriber: ReflectHandle<Deployment> = writer
        .subscribe()
        // safe unwrap: writer is created from a shared store
        .expect("subscribers can only be created from shared stores");

    let (reload_tx, reload_rx) = futures::channel::mpsc::channel(RELOAD_BUFFER_SIZE);

    let deployment = Api::<Deployment>::all(client.clone());

    let deployment_watch = watcher(
        deployment.clone(),
        watcher::Config::default().labels("app.kubernetes.io/managed-by=kaniop"),
    )
    .default_backoff()
    .reflect_shared(writer)
    .for_each(|res| {
        let mut reload_tx_clone = reload_tx.clone();
        async move {
            match res {
                Ok(event) => {
                    debug!("watched event");
                    match event {
                        watcher::Event::Delete(d) => {
                            debug!(
                                msg = "deleted deployment",
                                // safe unwrap: deployment is a namespace scoped resource
                                namespace = d.namespace().unwrap(),
                                name = d.name_any()
                            );
                            // trigger reconcile on delete for echo from owner reference
                            reload_tx_clone.try_send(()).unwrap();
                        }
                        watcher::Event::Apply(d) => {
                            debug!(
                                msg = "added or modified deployment",
                                // safe unwrap: deployment is a namespace scoped resource
                                namespace = d.namespace().unwrap(),
                                name = d.name_any()
                            );
                        }
                        _ => {}
                    }
                }
                Err(e) => error!(msg = "unexpected error when watching resource", %e),
            }
        }
    });

    info!(msg = "starting echo controller");
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    let echo_controller = Controller::new(echo, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(subscriber)
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(
            reconcile_echo,
            error_policy,
            state.to_context(client, deployment_store),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    tokio::select! {
        _ = echo_controller => {},
        _ = deployment_watch => {}
    }
}