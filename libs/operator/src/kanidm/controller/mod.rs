pub mod context;

use super::controller::context::{Context, Stores};
use super::crd::Kanidm;
use super::reconcile::reconcile_kanidm;

use crate::controller::{
    check_api_queryable, create_subscriber, ControllerId, ResourceReflector, State,
};
use crate::error::Error;
use crate::{backoff_reconciler, metrics};

use kaniop_k8s_util::types::short_type_name;

use std::fmt::Debug;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Namespace, Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ResourceExt};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::Lookup;
use kube::runtime::{watcher, WatchStreamExt};
use kube::Resource;
use serde::de::DeserializeOwned;
use tokio::time::Duration;
use tracing::{debug, error, info, trace};

pub const CONTROLLER_ID: ControllerId = "kanidm";
pub const SUBSCRIBE_BUFFER_SIZE: usize = 256;

const RELOAD_BUFFER_SIZE: usize = 16;

fn create_watcher<K>(
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
                            ctx.kaniop_ctx
                                .metrics
                                .triggered_inc(metrics::Action::Delete, resource_name);
                        }
                        watcher::Event::Apply(d) => {
                            debug!(
                                msg = format!("apply event for {resource_name} trigger reconcile"),
                                namespace = ResourceExt::namespace(&d).unwrap(),
                                name = d.name_any()
                            );
                            ctx.kaniop_ctx
                                .metrics
                                .triggered_inc(metrics::Action::Apply, resource_name);
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    error!(msg = format!("unexpected error when watching {resource_name}"), %e);
                    ctx.kaniop_ctx.metrics.watch_operations_failed_inc();
                }
            }
        }
    })
    .boxed()
}

/// Initialize Kanidm controller and shared state
pub async fn run(
    state: State,
    client: Client,
    namespace_api: Api<Namespace>,
    namespace_r: ResourceReflector<Namespace>,
    kanidm_api: Api<Kanidm>,
    kanidm_r: ResourceReflector<Kanidm>,
) {
    let statefulset = check_api_queryable::<StatefulSet>(client.clone()).await;
    let service = check_api_queryable::<Service>(client.clone()).await;
    let ingress = check_api_queryable::<Ingress>(client.clone()).await;
    let secret = check_api_queryable::<Secret>(client.clone()).await;

    let statefulset_r = create_subscriber::<StatefulSet>(SUBSCRIBE_BUFFER_SIZE);
    let service_r = create_subscriber::<Service>(SUBSCRIBE_BUFFER_SIZE);
    let ingress_r = create_subscriber::<Ingress>(SUBSCRIBE_BUFFER_SIZE);
    let secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);

    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);

    let stores = Stores {
        stateful_set_store: statefulset_r.store,
        service_store: service_r.store,
        ingress_store: ingress_r.store,
        secret_store: secret_r.store,
    };

    let ctx = Arc::new(Context::new(
        state.to_context(client, CONTROLLER_ID),
        stores,
    ));
    let statefulset_watcher = create_watcher(
        statefulset,
        statefulset_r.writer,
        reload_tx.clone(),
        ctx.clone(),
    );
    let service_watcher = create_watcher(service, service_r.writer, reload_tx.clone(), ctx.clone());
    let ingress_watcher = create_watcher(ingress, ingress_r.writer, reload_tx.clone(), ctx.clone());
    let secret_watcher = create_watcher(secret, secret_r.writer, reload_tx.clone(), ctx.clone());

    let namespace_watcher = watcher(namespace_api, watcher::Config::default().any_semantic())
        .default_backoff()
        .reflect(namespace_r.writer)
        .for_each(|res| {
            let ctx = ctx.clone();
            async move {
                match res {
                    Ok(event) => {
                        trace!(msg = format!("receive namespace event: {event:?}"),)
                    }
                    Err(e) => {
                        error!(msg = format!("unexpected error when watching namespace"), %e);
                        ctx.kaniop_ctx.metrics.watch_operations_failed_inc();
                    }
                }
            }
        });

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    // https://kubernetes.io/docs/reference/using-api/api-concepts/#streaming-lists
    let kanidm_watcher = watcher(kanidm_api, watcher::Config::default().any_semantic())
        .default_backoff()
        .reflect(kanidm_r.writer)
        .touched_objects();

    let kanidm_controller = Controller::for_stream(kanidm_watcher, kanidm_r.store)
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(statefulset_r.subscriber)
        .owns_shared_stream(service_r.subscriber)
        .owns_shared_stream(ingress_r.subscriber)
        .owns_shared_stream(secret_r.subscriber)
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_kanidm),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.kaniop_ctx.metrics.ready_set(1);
    tokio::select! {
        _ = kanidm_controller => {},
        _ = namespace_watcher => {},
        _ = statefulset_watcher => {},
        _ = service_watcher => {},
        _ = ingress_watcher => {},
        _ = secret_watcher => {},
    }
}
