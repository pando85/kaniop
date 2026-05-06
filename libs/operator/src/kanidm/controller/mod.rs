pub mod context;

use super::controller::context::{Context, Stores};
use super::crd::Kanidm;
use super::reconcile::{
    reconcile_kanidm,
    secret::{SECRET_TYPE_LABEL, SecretType},
};

use crate::backoff_reconciler;
use crate::controller::{
    ControllerId, RELOAD_BUFFER_SIZE, ResourceReflector, SUBSCRIBE_BUFFER_SIZE, State,
    check_api_queryable, check_api_queryable_optional, create_subscriber, create_watcher,
};
use kaniop_k8s_util::error::Error;

use std::sync::Arc;

use futures::FutureExt;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use gateway_api::apis::standard::httproutes::HTTPRoute;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Namespace, Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::ResourceExt;
use kube::api::Api;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{ReflectHandle, Store};
use kube::runtime::{WatchStreamExt, watcher};
use tokio::time::Duration;
use tracing::{debug, error, info, trace};

pub const CONTROLLER_ID: ControllerId = "kanidm";

fn create_replica_cert_watcher(
    api: Api<Secret>,
    writer: Writer<Secret>,
    ctx: Arc<Context>,
) -> BoxFuture<'static, ()> {
    let replica_cert_label = serde_plain::to_string(&SecretType::ReplicaCert).unwrap();

    watcher(
        api,
        watcher::Config::default().labels(&format!("{}={}", SECRET_TYPE_LABEL, replica_cert_label)),
    )
    .default_backoff()
    .reflect_shared(writer)
    .for_each(move |res| {
        let ctx = ctx.clone();
        async move {
            match res {
                Ok(event) => {
                    trace!(msg = "watched replica cert event", ?event);
                    match event {
                        watcher::Event::InitApply(secret) | watcher::Event::Apply(secret) => {
                            debug!(
                                msg = "init or apply event for replica cert secret trigger reconcile",
                                namespace = secret.namespace().unwrap(),
                                name = secret.name_any()
                            );
                            let _ignore_errors = ctx.insert_repl_cert_exp(&secret).await.map_err(
                                |e| error!(msg = "failed to get replica cert expiration, automatic certificate renewal may be affected", %e),
                            );
                        }
                        watcher::Event::Delete(secret) => {
                            debug!(
                                msg = "delete event for replica cert secret",
                                namespace = secret.namespace().unwrap(),
                                name = secret.name_any()
                            );
                            ctx.remove_repl_cert_exp(&ObjectRef::from(&secret))
                                .await;
                            ctx.remove_repl_cert_host(&ObjectRef::from(&secret))
                                .await;
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    error!(msg = "unexpected error when watching replica cert secrets", %e);
                    ctx.kaniop_ctx.metrics.watch_operations_failed_inc();
                }
            }
        }
    })
    .boxed()
}

#[allow(clippy::too_many_arguments)]
fn run_controller(
    kanidm_watcher: impl futures::Stream<Item = Result<Kanidm, watcher::Error>> + Send + 'static,
    kanidm_store: Store<Kanidm>,
    statefulset_subscriber: ReflectHandle<StatefulSet>,
    service_subscriber: ReflectHandle<Service>,
    ingress_subscriber: ReflectHandle<Ingress>,
    secret_subscriber: ReflectHandle<Secret>,
    replica_cert_secret_subscriber: ReflectHandle<Secret>,
    http_route_subscriber: Option<ReflectHandle<HTTPRoute>>,
    reload_rx: mpsc::Receiver<()>,
    ctx: Arc<Context>,
) -> BoxFuture<'static, ()> {
    let mut controller = Controller::for_stream(kanidm_watcher, kanidm_store)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(statefulset_subscriber)
        .owns_shared_stream(service_subscriber)
        .owns_shared_stream(ingress_subscriber)
        .owns_shared_stream(secret_subscriber)
        .owns_shared_stream(replica_cert_secret_subscriber);

    if let Some(subscriber) = http_route_subscriber {
        controller = controller.owns_shared_stream(subscriber);
    }

    controller
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_kanidm),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx,
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
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
    let http_route_api = check_api_queryable_optional::<HTTPRoute>(client.clone()).await;

    let statefulset_r = create_subscriber::<StatefulSet>(SUBSCRIBE_BUFFER_SIZE);
    let service_r = create_subscriber::<Service>(SUBSCRIBE_BUFFER_SIZE);
    let ingress_r = create_subscriber::<Ingress>(SUBSCRIBE_BUFFER_SIZE);
    let secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);
    let replica_cert_secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);

    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);

    let http_route_r = if http_route_api.is_some() {
        Some(create_subscriber::<HTTPRoute>(SUBSCRIBE_BUFFER_SIZE))
    } else {
        None
    };

    let stores = Stores {
        stateful_set_store: statefulset_r.store,
        service_store: service_r.store,
        ingress_store: ingress_r.store,
        secret_store: secret_r.store,
        http_route_store: http_route_r.as_ref().map(|r| r.store.clone()),
    };

    let ctx = Arc::new(Context::new(
        state.to_context(client, CONTROLLER_ID),
        stores,
    ));
    let kaniop_ctx = Arc::new(ctx.kaniop_ctx.clone());

    let statefulset_watcher = create_watcher(
        statefulset,
        statefulset_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );
    let service_watcher = create_watcher(
        service,
        service_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );
    let ingress_watcher = create_watcher(
        ingress,
        ingress_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );
    let secret_watcher = create_watcher(
        secret.clone(),
        secret_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );

    let replica_cert_secrets_watcher =
        create_replica_cert_watcher(secret, replica_cert_secret_r.writer, ctx.clone());

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
    let kanidm_watcher = watcher(kanidm_api, watcher::Config::default().any_semantic())
        .default_backoff()
        .reflect(kanidm_r.writer)
        .touched_objects();

    let (http_route_watcher, http_route_subscriber) = match (http_route_api, http_route_r) {
        (Some(api), Some(r)) => {
            let watcher = create_watcher(api, r.writer, reload_tx, CONTROLLER_ID, kaniop_ctx);
            (Some(watcher), Some(r.subscriber))
        }
        _ => (None, None),
    };

    let kanidm_controller = run_controller(
        kanidm_watcher,
        kanidm_r.store,
        statefulset_r.subscriber,
        service_r.subscriber,
        ingress_r.subscriber,
        secret_r.subscriber,
        replica_cert_secret_r.subscriber,
        http_route_subscriber,
        reload_rx,
        ctx.clone(),
    );

    ctx.kaniop_ctx.metrics.ready_set(1);
    tokio::select! {
        _ = kanidm_controller => {},
        _ = namespace_watcher => {},
        _ = statefulset_watcher => {},
        _ = service_watcher => {},
        _ = ingress_watcher => {},
        _ = secret_watcher => {},
        _ = http_route_watcher.unwrap_or(futures::future::pending().boxed()) => {},
        _ = replica_cert_secrets_watcher => {},
    }
}
