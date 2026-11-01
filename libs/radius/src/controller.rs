//! KanidmRadiusServer controller wiring.
//!
//! Owned objects (Deployments, Services, and config Secrets through a
//! metadata-only watcher) trigger their owner. Referenced inputs carry no
//! Kaniop label, so metadata-only watchers over all Secrets and ConfigMaps
//! are mapped by namespace and name to the RADIUS servers that reference
//! them; their data is never cached. Changes of a referenced `Kanidm` spec
//! (origin, domain, image, namespace selector) trigger every RADIUS server
//! pointing at it.
//!
//! kube-runtime never runs two reconciles of the same object at once, which
//! the token executors rely on: `generate_api_token` returns only the bearer
//! and the new token id is found by listing before and after the mint.

use std::sync::Arc;

use futures::channel::mpsc;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret, Service};
use kanidm_client::KanidmClient;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::context::{
    BackoffContext, Context as KaniopContext, IdmClientContext,
};
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::controller::{
    ControllerId, RELOAD_BUFFER_SIZE, SUBSCRIBE_BUFFER_SIZE, State, check_api_queryable,
    check_api_queryable_optional, create_metadata_watcher, create_subscriber, create_watcher,
};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::metrics::ControllerMetrics;
use kube::ResourceExt;
use kube::api::{Api, PartialObjectMeta};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::{WatchStreamExt, predicates, watcher};
use tokio::time::Duration;
use tracing::{error, info};

use crate::crd::{KanidmRadiusServer, RadiusCaKind};
use crate::reconcile::cleanup::DrainWarnings;
use crate::reconcile::reconcile_radius;

pub const CONTROLLER_ID: ControllerId = "radius-server";

#[derive(Clone)]
pub struct Context {
    pub kaniop_ctx: KaniopContext<KanidmRadiusServer>,
    /// Our Deployments (cache; status reporting only, never token decisions).
    pub deployment_store: Store<Deployment>,
    /// Our Services (cache; status reporting only).
    pub service_store: Store<Service>,
    /// Rate limit of the finalizer's drain warning events.
    pub drain_warnings: Arc<DrainWarnings>,
}

impl BackoffContext<KanidmRadiusServer> for Context {
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        self.kaniop_ctx.metrics()
    }

    async fn get_backoff(&self, obj_ref: ObjectRef<KanidmRadiusServer>) -> Duration {
        self.kaniop_ctx.get_backoff(obj_ref).await
    }

    async fn reset_backoff(&self, obj_ref: ObjectRef<KanidmRadiusServer>) {
        self.kaniop_ctx.reset_backoff(obj_ref).await
    }
}

impl IdmClientContext<KanidmRadiusServer> for Context {
    async fn get_idm_client(&self, obj: &KanidmRadiusServer) -> Result<Arc<KanidmClient>> {
        self.kaniop_ctx.get_idm_client(obj).await
    }
}

/// RADIUS servers in `namespace` that reference the Secret `name` (NAS
/// shared secret, EAP TLS Secret, or a Secret-kind Kanidm CA).
pub fn radius_for_secret(
    radius: &[Arc<KanidmRadiusServer>],
    namespace: &str,
    name: &str,
) -> Vec<ObjectRef<KanidmRadiusServer>> {
    referencing(radius, namespace, |r| {
        r.spec
            .clients
            .iter()
            .any(|c| c.shared_secret_ref.name == name)
            || r.spec.radius_tls.secret_ref.name == name
            || kanidm_ca(r, RadiusCaKind::Secret) == Some(name)
    })
}

/// RADIUS servers in `namespace` whose Kanidm CA is the ConfigMap `name`.
pub fn radius_for_config_map(
    radius: &[Arc<KanidmRadiusServer>],
    namespace: &str,
    name: &str,
) -> Vec<ObjectRef<KanidmRadiusServer>> {
    referencing(radius, namespace, |r| {
        kanidm_ca(r, RadiusCaKind::ConfigMap) == Some(name)
    })
}

/// RADIUS servers whose `kanidmRef` resolves to `namespace/name`.
pub fn radius_for_kanidm(
    radius: &[Arc<KanidmRadiusServer>],
    namespace: &str,
    name: &str,
) -> Vec<ObjectRef<KanidmRadiusServer>> {
    radius
        .iter()
        .filter(|r| r.namespace().is_some())
        .filter(|r| r.kanidm_namespace() == namespace && r.kanidm_name() == name)
        .map(|r| ObjectRef::from_obj(r.as_ref()))
        .collect()
}

/// Inputs are same-namespace references only.
fn referencing(
    radius: &[Arc<KanidmRadiusServer>],
    namespace: &str,
    references: impl Fn(&KanidmRadiusServer) -> bool,
) -> Vec<ObjectRef<KanidmRadiusServer>> {
    radius
        .iter()
        .filter(|r| r.namespace().as_deref() == Some(namespace))
        .filter(|r| references(r))
        .map(|r| ObjectRef::from_obj(r.as_ref()))
        .collect()
}

fn kanidm_ca(radius: &KanidmRadiusServer, kind: RadiusCaKind) -> Option<&str> {
    radius
        .spec
        .kanidm_tls
        .as_ref()
        .and_then(|tls| tls.ca_bundle_ref.as_ref())
        .filter(|ca| ca.kind == kind)
        .map(|ca| ca.name.as_str())
}

/// Start the controller. Returns immediately (controller disabled) when the
/// CRD is not installed.
pub async fn run(state: State, client: Client) {
    let Some(radius_api) = check_api_queryable_optional::<KanidmRadiusServer>(client.clone()).await
    else {
        info!(
            controller = CONTROLLER_ID,
            "KanidmRadiusServer CRD unavailable; controller disabled"
        );
        return;
    };
    let deployment = check_api_queryable::<Deployment>(client.clone()).await;
    let service = check_api_queryable::<Service>(client.clone()).await;
    let owned_secret = check_api_queryable::<PartialObjectMeta<Secret>>(client.clone()).await;

    let deployment_r = create_subscriber::<Deployment>(SUBSCRIBE_BUFFER_SIZE);
    let service_r = create_subscriber::<Service>(SUBSCRIBE_BUFFER_SIZE);
    let secret_r = create_subscriber::<PartialObjectMeta<Secret>>(SUBSCRIBE_BUFFER_SIZE);
    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);

    let ctx = Arc::new(Context {
        kaniop_ctx: state.to_context(client.clone(), CONTROLLER_ID),
        deployment_store: deployment_r.store,
        service_store: service_r.store,
        drain_warnings: Arc::default(),
    });
    let kaniop_ctx = Arc::new(ctx.kaniop_ctx.clone());

    let deployment_watcher = create_watcher(
        deployment,
        deployment_r.writer,
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
    let secret_watcher = create_metadata_watcher(
        owned_secret,
        secret_r.writer,
        reload_tx,
        CONTROLLER_ID,
        kaniop_ctx.clone(),
    );

    // Referenced inputs: metadata only, every namespace, no cache of data.
    let input_secret_metrics = kaniop_ctx.clone();
    let input_secret_trigger = watcher(
        Api::<PartialObjectMeta<Secret>>::all(client.clone()),
        watcher::Config::default().any_semantic(),
    )
    .default_backoff()
    .touched_objects()
    .inspect_err(move |e| {
        error!(error = %e, "unexpected error when watching referenced RADIUS Secrets");
        input_secret_metrics.metrics.watch_operations_failed_inc();
    });
    let input_config_map_metrics = kaniop_ctx.clone();
    let input_config_map_trigger = watcher(
        Api::<PartialObjectMeta<ConfigMap>>::all(client.clone()),
        watcher::Config::default().any_semantic(),
    )
    .default_backoff()
    .touched_objects()
    .inspect_err(move |e| {
        error!(error = %e, "unexpected error when watching referenced RADIUS ConfigMaps");
        input_config_map_metrics
            .metrics
            .watch_operations_failed_inc();
    });
    // Only spec changes of a Kanidm matter here; its status churn does not.
    let kanidm_metrics = kaniop_ctx;
    let kanidm_trigger = watcher(
        Api::<Kanidm>::all(client.clone()),
        watcher::Config::default().any_semantic(),
    )
    .default_backoff()
    .touched_objects()
    .predicate_filter(predicates::generation, Default::default())
    .inspect_err(move |e| {
        error!(error = %e, "unexpected error when watching Kanidm for RADIUS");
        kanidm_metrics.metrics.watch_operations_failed_inc();
    });

    let controller = Controller::new(radius_api, watcher::Config::default().any_semantic());
    let radius_store = controller.store();
    let secret_store = radius_store.clone();
    let config_map_store = radius_store.clone();
    let kanidm_store = radius_store;

    info!(controller = CONTROLLER_ID, "starting controller");
    let radius_controller = controller
        // debounce to filter out reconcile calls that happen in quick succession
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(deployment_r.subscriber)
        .owns_shared_stream(service_r.subscriber)
        .owns_shared_stream(secret_r.subscriber)
        .watches_stream(input_secret_trigger, move |secret| {
            radius_for_secret(
                &secret_store.state(),
                &secret.namespace().unwrap_or_default(),
                &secret.name_any(),
            )
        })
        .watches_stream(input_config_map_trigger, move |config_map| {
            radius_for_config_map(
                &config_map_store.state(),
                &config_map.namespace().unwrap_or_default(),
                &config_map.name_any(),
            )
        })
        .watches_stream(kanidm_trigger, move |kanidm| {
            radius_for_kanidm(
                &kanidm_store.state(),
                &kanidm.namespace().unwrap_or_default(),
                &kanidm.name_any(),
            )
        })
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_radius),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.kaniop_ctx.metrics.ready_set(1);
    tokio::select! {
        _ = radius_controller => {},
        _ = deployment_watcher => {},
        _ = service_watcher => {},
        _ = secret_watcher => {},
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn radius(
        namespace: &str,
        name: &str,
        kanidm: serde_json::Value,
        ca: Option<(&str, &str)>,
    ) -> Arc<KanidmRadiusServer> {
        let mut spec = json!({
            "kanidmRef": kanidm,
            "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
            "clients": [
                {"name": "a", "ipaddr": "192.0.2.1", "sharedSecretRef": {"name": "nas-a", "key": "a"}},
                {"name": "b", "ipaddr": "192.0.2.2", "sharedSecretRef": {"name": "nas-shared", "key": "b"}}
            ],
            "radiusTls": {"secretRef": {"name": "eap-tls"}}
        });
        if let Some((kind, ca_name)) = ca {
            spec["kanidmTls"] =
                json!({"caBundleRef": {"kind": kind, "name": ca_name, "key": "ca.crt"}});
        }
        Arc::new(
            serde_json::from_value(json!({
                "apiVersion": "kaniop.rs/v1beta1",
                "kind": "KanidmRadiusServer",
                "metadata": {"name": name, "namespace": namespace},
                "spec": spec
            }))
            .unwrap(),
        )
    }

    fn names(refs: Vec<ObjectRef<KanidmRadiusServer>>) -> Vec<String> {
        let mut names: Vec<_> = refs
            .into_iter()
            .map(|r| format!("{}/{}", r.namespace.unwrap(), r.name))
            .collect();
        names.sort();
        names
    }

    fn store() -> Vec<Arc<KanidmRadiusServer>> {
        vec![
            radius(
                "net",
                "one",
                json!({"name": "idm"}),
                Some(("Secret", "private-ca")),
            ),
            radius(
                "net",
                "two",
                json!({"name": "idm", "namespace": "identity"}),
                Some(("ConfigMap", "kanidm-ca")),
            ),
            radius(
                "other",
                "three",
                json!({"name": "idm", "namespace": "net"}),
                None,
            ),
        ]
    }

    #[test]
    fn controller_id_is_stable_for_metrics() {
        assert_eq!(CONTROLLER_ID, "radius-server");
    }

    #[test]
    fn secrets_map_to_referencing_servers_in_their_namespace() {
        let store = store();
        assert_eq!(
            names(radius_for_secret(&store, "net", "nas-a")),
            ["net/one", "net/two"]
        );
        assert_eq!(
            names(radius_for_secret(&store, "net", "nas-shared")),
            ["net/one", "net/two"]
        );
        assert_eq!(
            names(radius_for_secret(&store, "net", "eap-tls")),
            ["net/one", "net/two"]
        );
        assert_eq!(
            names(radius_for_secret(&store, "other", "eap-tls")),
            ["other/three"]
        );
        // A Secret-kind CA maps; a ConfigMap-kind CA name does not match Secrets.
        assert_eq!(
            names(radius_for_secret(&store, "net", "private-ca")),
            ["net/one"]
        );
        assert!(radius_for_secret(&store, "net", "kanidm-ca").is_empty());
        assert!(radius_for_secret(&store, "net", "unrelated").is_empty());
        assert!(radius_for_secret(&store, "elsewhere", "nas-a").is_empty());
    }

    #[test]
    fn config_maps_map_only_to_config_map_cas() {
        let store = store();
        assert_eq!(
            names(radius_for_config_map(&store, "net", "kanidm-ca")),
            ["net/two"]
        );
        assert!(radius_for_config_map(&store, "net", "private-ca").is_empty());
        assert!(radius_for_config_map(&store, "other", "kanidm-ca").is_empty());
        assert!(radius_for_config_map(&store, "net", "eap-tls").is_empty());
    }

    #[test]
    fn kanidm_maps_to_servers_by_resolved_reference() {
        let store = store();
        // `one` has no namespace in its ref: it resolves to its own namespace.
        assert_eq!(
            names(radius_for_kanidm(&store, "net", "idm")),
            ["net/one", "other/three"]
        );
        assert_eq!(
            names(radius_for_kanidm(&store, "identity", "idm")),
            ["net/two"]
        );
        assert!(radius_for_kanidm(&store, "other", "idm").is_empty());
        assert!(radius_for_kanidm(&store, "net", "idm2").is_empty());
    }
}
