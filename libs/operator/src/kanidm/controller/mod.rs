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
use futures::TryStreamExt;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use gateway_api::apis::standard::backendtlspolicies::BackendTLSPolicy;
use gateway_api::apis::standard::httproutes::HTTPRoute;
use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::ResourceExt;
use kube::api::{Api, PartialObjectMeta};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{ReflectHandle, Store};
use kube::runtime::{WatchStreamExt, metadata_watcher, watcher};
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

/// Map a TLS secret event to the Kanidm objects that mount it.
///
/// The TLS secret is user-provided (e.g. created by cert-manager), so it
/// carries neither kaniop labels nor owner references pointing to the Kanidm:
/// match by namespace and effective TLS secret name against the Kanidm store.
fn map_tls_secret_to_kanidms(
    kanidm_store: &Store<Kanidm>,
    secret: &PartialObjectMeta<Secret>,
) -> Vec<ObjectRef<Kanidm>> {
    kanidm_store
        .state()
        .into_iter()
        .filter(|kanidm| {
            kanidm.namespace() == secret.namespace()
                && kanidm.effective_tls_secret_name() == secret.name_any()
        })
        .map(|kanidm| ObjectRef::from(kanidm.as_ref()))
        .collect()
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
    tls_secret_trigger: impl futures::Stream<Item = Result<PartialObjectMeta<Secret>, watcher::Error>>
    + Send
    + 'static,
    http_route_subscriber: Option<ReflectHandle<HTTPRoute>>,
    backend_tls_policy_subscriber: Option<ReflectHandle<BackendTLSPolicy>>,
    deployment_subscriber: ReflectHandle<Deployment>,
    config_map_subscriber: ReflectHandle<ConfigMap>,
    reload_rx: mpsc::Receiver<()>,
    ctx: Arc<Context>,
) -> BoxFuture<'static, ()> {
    let kanidm_store_for_tls = kanidm_store.clone();
    let mut controller = Controller::for_stream(kanidm_watcher, kanidm_store)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(statefulset_subscriber)
        .owns_shared_stream(service_subscriber)
        .owns_shared_stream(ingress_subscriber)
        .owns_shared_stream(secret_subscriber)
        .owns_shared_stream(replica_cert_secret_subscriber)
        .owns_shared_stream(deployment_subscriber)
        .owns_shared_stream(config_map_subscriber)
        .watches_stream(tls_secret_trigger, move |secret| {
            map_tls_secret_to_kanidms(&kanidm_store_for_tls, &secret)
        });

    if let Some(subscriber) = http_route_subscriber {
        controller = controller.owns_shared_stream(subscriber);
    }

    if let Some(subscriber) = backend_tls_policy_subscriber {
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
    let backend_tls_policy_api =
        check_api_queryable_optional::<BackendTLSPolicy>(client.clone()).await;
    let deployment = check_api_queryable::<Deployment>(client.clone()).await;
    let config_map = check_api_queryable::<ConfigMap>(client.clone()).await;

    let statefulset_r = create_subscriber::<StatefulSet>(SUBSCRIBE_BUFFER_SIZE);
    let service_r = create_subscriber::<Service>(SUBSCRIBE_BUFFER_SIZE);
    let ingress_r = create_subscriber::<Ingress>(SUBSCRIBE_BUFFER_SIZE);
    let secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);
    let replica_cert_secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);
    let deployment_r = create_subscriber::<Deployment>(SUBSCRIBE_BUFFER_SIZE);
    let config_map_r = create_subscriber::<ConfigMap>(SUBSCRIBE_BUFFER_SIZE);

    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);

    let http_route_r = if http_route_api.is_some() {
        Some(create_subscriber::<HTTPRoute>(SUBSCRIBE_BUFFER_SIZE))
    } else {
        None
    };

    let backend_tls_policy_r = if backend_tls_policy_api.is_some() {
        Some(create_subscriber::<BackendTLSPolicy>(SUBSCRIBE_BUFFER_SIZE))
    } else {
        None
    };

    let stores = Stores {
        stateful_set_store: statefulset_r.store,
        service_store: service_r.store,
        ingress_store: ingress_r.store,
        secret_store: secret_r.store,
        http_route_store: http_route_r.as_ref().map(|r| r.store.clone()),
        backend_tls_policy_store: backend_tls_policy_r.as_ref().map(|r| r.store.clone()),
        deployment_store: deployment_r.store,
        config_map_store: config_map_r.store,
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
    let deployment_watcher = create_watcher(
        deployment,
        deployment_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );
    let config_map_watcher = create_watcher(
        config_map,
        config_map_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );

    let replica_cert_secrets_watcher =
        create_replica_cert_watcher(secret.clone(), replica_cert_secret_r.writer, ctx.clone());

    // TLS secrets are user-provided (e.g. created by cert-manager) and carry no
    // kaniop label to filter on: watch metadata of all secrets and let the
    // mapper match them by name against the Kanidm store.
    let tls_secret_metrics_ctx = kaniop_ctx.clone();
    let tls_secret_trigger = metadata_watcher(secret, watcher::Config::default().any_semantic())
        .default_backoff()
        .touched_objects()
        .inspect_err(move |e| {
            error!(msg = "unexpected error when watching TLS secrets", %e);
            tls_secret_metrics_ctx.metrics.watch_operations_failed_inc();
        });

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
            let watcher = create_watcher(
                api,
                r.writer,
                reload_tx.clone(),
                CONTROLLER_ID,
                kaniop_ctx.clone(),
            );
            (Some(watcher), Some(r.subscriber))
        }
        _ => (None, None),
    };

    let (backend_tls_policy_watcher, backend_tls_policy_subscriber) =
        match (backend_tls_policy_api, backend_tls_policy_r) {
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
        tls_secret_trigger,
        http_route_subscriber,
        backend_tls_policy_subscriber,
        deployment_r.subscriber,
        config_map_r.subscriber,
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
        _ = backend_tls_policy_watcher.unwrap_or(futures::future::pending().boxed()) => {},
        _ = replica_cert_secrets_watcher => {},
        _ = deployment_watcher => {},
        _ = config_map_watcher => {},
    }
}

#[cfg(test)]
mod tests {
    use super::map_tls_secret_to_kanidms;
    use crate::kanidm::crd::{Kanidm, KanidmSpec};

    use k8s_openapi::api::core::v1::Secret;
    use kube::api::{ObjectMeta, PartialObjectMeta};
    use kube::runtime::reflector;
    use kube::runtime::watcher;

    fn kanidm(name: &str, namespace: &str, tls_secret_name: Option<&str>) -> Kanidm {
        Kanidm {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            spec: KanidmSpec {
                domain: "idm.example.com".to_string(),
                tls_secret_name: tls_secret_name.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn secret_meta(name: &str, namespace: &str) -> PartialObjectMeta<Secret> {
        PartialObjectMeta {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_map_tls_secret_to_kanidms() {
        let (store, mut writer) = reflector::store();
        writer.apply_watcher_event(&watcher::Event::Apply(kanidm("a", "ns1", None)));
        writer.apply_watcher_event(&watcher::Event::Apply(kanidm(
            "b",
            "ns1",
            Some("custom-tls"),
        )));
        writer.apply_watcher_event(&watcher::Event::Apply(kanidm("c", "ns2", None)));

        // default secret name `{name}-tls`
        let refs = map_tls_secret_to_kanidms(&store, &secret_meta("a-tls", "ns1"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "a");

        // explicit `tlsSecretName`
        let refs = map_tls_secret_to_kanidms(&store, &secret_meta("custom-tls", "ns1"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "b");

        // same name in another namespace does not match
        assert!(map_tls_secret_to_kanidms(&store, &secret_meta("a-tls", "ns2")).is_empty());

        // unrelated secret matches nothing
        assert!(map_tls_secret_to_kanidms(&store, &secret_meta("other", "ns1")).is_empty());
    }
}
