use crate::crd::Kanidm;
use crate::reconcile::reconcile_kanidm;

use kaniop_k8s_util::types::short_type_name;
use kaniop_operator::controller::{Context, ControllerId, ResourceReflector, State, Stores};
use kaniop_operator::error::Error;
use kaniop_operator::metrics;

use std::fmt::Debug;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ListParams, ResourceExt};
use kube::client::Client;
use kube::runtime::controller::{self, Action, Controller};
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{self, Lookup};
use kube::runtime::{watcher, WatchStreamExt};
use kube::Resource;
use serde::de::DeserializeOwned;
use tokio::time::Duration;
use tracing::{debug, error, info, trace};

pub const CONTROLLER_ID: ControllerId = "kanidm";

const SUBSCRIBE_BUFFER_SIZE: usize = 256;
const RELOAD_BUFFER_SIZE: usize = 16;

fn error_policy<K: ResourceExt>(obj: Arc<K>, error: &Error, ctx: Arc<Context>) -> Action {
    // safe unwrap: statefulset is a namespace scoped resource
    error!(msg = "failed reconciliation", namespace = %obj.namespace().unwrap(), name = %obj.name_any(), %error);
    ctx.metrics.reconcile_failure_inc();
    Action::requeue(Duration::from_secs(5 * 60))
}

async fn check_api_queryable<K>(client: Client) -> Api<K>
where
    K: Resource + Clone + DeserializeOwned + Debug,
    <K as Resource>::DynamicType: Default,
{
    let api = Api::<K>::all(client.clone());
    if let Err(e) = api.list(&ListParams::default().limit(1)).await {
        error!(
            "{} is not queryable; {e:?}. Check controller permissions",
            short_type_name::<K>().unwrap_or("Unknown resource"),
        );
        std::process::exit(1);
    }
    api
}

fn create_subscriber<K>(buffer_size: usize) -> ResourceReflector<K>
where
    K: Resource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    let (store, writer) = reflector::store_shared(buffer_size);
    let subscriber = writer
        .subscribe()
        .expect("subscribers can only be created from shared stores");

    ResourceReflector {
        store,
        writer,
        subscriber,
    }
}

fn create_watch<K>(
    api: Api<K>,
    writer: Writer<K>,
    reload_tx: mpsc::Sender<()>,
    ctx: Arc<Context>,
) -> BoxFuture<'static, ()>
where
    K: Resource + Lookup + Clone + DeserializeOwned + Send + Sync + Debug + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone + Send + Sync,
    <K as Resource>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    let resource_name = short_type_name::<K>().unwrap_or("Unknown");

    watcher(
        api,
        watcher::Config::default().labels("app.kubernetes.io/managed-by=kaniop"),
    )
    .default_backoff()
    .reflect_shared(writer)
    .for_each(move |res| {
        let mut reload_tx_clone = reload_tx.clone();
        let ctx = ctx.clone();
        async move {
            match res {
                Ok(event) => {
                    trace!(msg = "watched event", ?event);
                    match event {
                        watcher::Event::Delete(d) => {
                            debug!(
                                msg = format!("delete event for {resource_name} trigger reconcile"),
                                namespace = ResourceExt::namespace(&d).unwrap(),
                                name = d.name_any()
                            );

                            // TODO: remove for each trigger on delete logic when
                            // (dispatch delete events issue)[https://github.com/kube-rs/kube/issues/1590]
                            // is solved
                            let _ignore_errors = reload_tx_clone.try_send(()).map_err(
                                |e| error!(msg = "failed to trigger reconcile on delete", %e),
                            );
                            ctx.metrics
                                .triggered_inc(metrics::Action::Delete, resource_name);
                        }
                        watcher::Event::Apply(d) => {
                            debug!(
                                msg = format!("apply event for {resource_name} trigger reconcile"),
                                namespace = ResourceExt::namespace(&d).unwrap(),
                                name = d.name_any()
                            );
                            ctx.metrics
                                .triggered_inc(metrics::Action::Apply, resource_name);
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    error!(msg = format!("unexpected error when watching {resource_name}"), %e);
                    ctx.metrics.watch_operations_failed_inc();
                }
            }
        }
    })
    .boxed()
}

/// Initialize Kanidm controller and shared state
pub async fn run(state: State, client: Client) {
    let kanidm = check_api_queryable::<Kanidm>(client.clone()).await;
    let statefulset = check_api_queryable::<StatefulSet>(client.clone()).await;
    let service = check_api_queryable::<Service>(client.clone()).await;
    let ingress = check_api_queryable::<Ingress>(client.clone()).await;
    let secret = check_api_queryable::<Secret>(client.clone()).await;

    let statefulset_r = create_subscriber::<StatefulSet>(SUBSCRIBE_BUFFER_SIZE);
    let service_r = create_subscriber::<Service>(SUBSCRIBE_BUFFER_SIZE);
    let ingress_r = create_subscriber::<Ingress>(SUBSCRIBE_BUFFER_SIZE);
    let secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);

    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);

    let stores = Stores::new(
        Some(statefulset_r.store),
        Some(service_r.store),
        Some(ingress_r.store),
        Some(secret_r.store),
    );
    let ctx = state.to_context(client, CONTROLLER_ID, stores);
    let statefulset_watch = create_watch(
        statefulset,
        statefulset_r.writer,
        reload_tx.clone(),
        ctx.clone(),
    );
    let service_watch = create_watch(service, service_r.writer, reload_tx.clone(), ctx.clone());
    let ingress_watch = create_watch(ingress, ingress_r.writer, reload_tx.clone(), ctx.clone());
    let secret_watch = create_watch(secret, secret_r.writer, reload_tx.clone(), ctx.clone());

    info!(msg = "starting kanidm controller");
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    let kanidm_controller = Controller::new(kanidm, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(statefulset_r.subscriber)
        .owns_shared_stream(service_r.subscriber)
        .owns_shared_stream(ingress_r.subscriber)
        .owns_shared_stream(secret_r.subscriber)
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(reconcile_kanidm, error_policy, ctx.clone())
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    tokio::select! {
        _ = kanidm_controller => {},
        _ = statefulset_watch => {},
        _ = service_watch => {},
        _ = ingress_watch => {},
        _ = secret_watch => {},
    }
}
