pub mod ingress;
pub mod secret;
pub mod service;
pub mod statefulset;
pub mod status;

use crate::crd::{Kanidm, KanidmReplicaState, KanidmStatus};
use crate::reconcile::ingress::IngressExt;
use crate::reconcile::secret::SecretExt;
use crate::reconcile::service::ServiceExt;
use crate::reconcile::statefulset::{StatefulSetExt, REPLICA_GROUP_LABEL};
use crate::reconcile::status::StatusExt;

use kaniop_k8s_util::client::get_output;
use kaniop_k8s_util::events::{Event, EventType};
use kaniop_k8s_util::types::short_type_name;
use kaniop_operator::controller::{Context, DEFAULT_RECONCILE_INTERVAL};
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;
use kube::runtime::reflector::ObjectRef;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

use futures::future::{join_all, try_join_all, TryJoinAll};
use futures::try_join;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::api::{Api, AttachParams, Patch, PatchParams, Resource};
use kube::core::NamespaceResourceScope;
use kube::runtime::controller::Action;
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use status::{is_kanidm_available, is_kanidm_initialized};
use tracing::{debug, field, info, instrument, trace, warn, Span};

pub const KANIDM_OPERATOR_NAME: &str = "kanidms.kaniop.rs";
pub const CLUSTER_LABEL: &str = "kanidm.kaniop.rs/cluster";
pub const INSTANCE_LABEL: &str = "app.kubernetes.io/instance";

static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "kanidm".to_string()),
        (
            "app.kubernetes.io/managed-by".to_string(),
            "kaniop".to_string(),
        ),
    ])
});

pub async fn reconcile_admins_secret(
    kanidm: Arc<Kanidm>,
    ctx: Arc<Context<Kanidm>>,
    status: &Result<KanidmStatus>,
) -> Result<()> {
    if let Ok(s) = status {
        if is_kanidm_available(s.clone()) && !is_kanidm_initialized(s.clone()) {
            let admins_secret = kanidm.generate_admins_secret(ctx.clone()).await?;
            kanidm.patch(ctx.clone(), admins_secret).await?;
        }
    }
    Ok(())
}

pub async fn reconcile_replication_secrets(
    kanidm: Arc<Kanidm>,
    ctx: Arc<Context<Kanidm>>,
    status: &Result<KanidmStatus>,
) -> Result<()> {
    if let Ok(s) = status {
        let secret_names = s
            .replica_statuses
            .iter()
            .map(|rs| kanidm.replica_secret_name(&rs.pod_name))
            .collect::<Vec<_>>();
        let secret_store = ctx.stores.secret_store();
        let deprecated_secrets = secret_store
            .state()
            .into_iter()
            .filter(|secret| {
                secret
                    .metadata
                    .labels
                    .iter()
                    .any(|l| l.get(CLUSTER_LABEL) == Some(&kanidm.name_any()))
                    && kanidm.admins_secret_name() != secret.name_any()
                    && secret_names.iter().all(|sn| sn != &secret.name_any())
            })
            .collect::<Vec<_>>();
        let secret_delete_future = deprecated_secrets
            .iter()
            .map(|secret| kanidm.delete(ctx.clone(), secret.as_ref()))
            .collect::<TryJoinAll<_>>();
        try_join!(secret_delete_future)?;

        if kanidm.is_replication_enabled() {
            let generate_secret_futures = s
                .replica_statuses
                .iter()
                .filter(|rs| rs.state == KanidmReplicaState::Pending)
                .map(|rs| kanidm.generate_replica_secret(ctx.clone(), &rs.pod_name))
                .collect::<Vec<_>>();
            let secrets = try_join_all(generate_secret_futures).await?;
            let secret_futures = secrets
                .into_iter()
                .map(|secret| kanidm.patch(ctx.clone(), secret))
                .collect::<Vec<_>>();
            try_join_all(secret_futures).await?;
            // TODO: rolling restart all of them one by one if you have write-replicas replica
            // group with one node
            let sts_api =
                Api::<StatefulSet>::namespaced(ctx.client.clone(), &kanidm.get_namespace());
            let sts_restart_futures = s
                .replica_statuses
                .iter()
                .filter(|rs| rs.state == KanidmReplicaState::Pending)
                .map(|rs| sts_api.restart(&rs.statefulset_name));
            let _ignore_errors = join_all(sts_restart_futures).await;
        }
    }
    Ok(())
}

async fn check_tls_secret(ctx: Arc<Context<Kanidm>>, kanidm: &Arc<Kanidm>) -> Result<()> {
    let secret_store = ctx.stores.secret_store();
    let secret_name = &kanidm.get_tls_secret_name();
    let namespace = kanidm.get_namespace();
    let tls_secret_ref = ObjectRef::<Secret>::new_with(secret_name, ()).within(&namespace);
    // TODO: create a metadata secret watcher filtering by `type=kubernetes.io/tls`
    if secret_store.get(&tls_secret_ref).is_none() {
        let msg = format!("TLS secret {secret_name} does not exist");
        ctx.recorder
            .publish(
                Event {
                    type_: EventType::Warning,
                    reason: "TlsSecretNotExists".to_string(),
                    note: Some(format!("{msg}. Create it before creating the Kanidm CRD.")),
                    action: "KanidmReconciling".into(),
                    secondary: None,
                },
                &kanidm.object_ref(&()),
            )
            .await
            .map_err(|e| {
                warn!(%msg, %e);
                Error::KubeError("failed to publish event".to_string(), e)
            })?;
        Err(Error::MissingData(format!(
            "{msg} in {namespace} namespace."
        )))
    } else {
        Ok(())
    }
}

#[instrument(skip(ctx, kanidm))]
pub async fn reconcile_kanidm(kanidm: Arc<Kanidm>, ctx: Arc<Context<Kanidm>>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling Kanidm");

    let status = kanidm.update_status(ctx.clone()).await.map_err(|e| {
        debug!(msg = "failed to reconcile status", %e);
        ctx.metrics.status_update_errors_inc();
        e
    });
    check_tls_secret(ctx.clone(), &kanidm).await?;
    let admin_secret_future = reconcile_admins_secret(kanidm.clone(), ctx.clone(), &status);
    let replication_secret_future =
        reconcile_replication_secrets(kanidm.clone(), ctx.clone(), &status);

    let services_per_pod_futures = match &status {
        Ok(s) if kanidm.is_replication_enabled() => {
            let futures = s
                .replica_statuses
                .iter()
                .map(|rs| {
                    let pod_name = rs.pod_name.clone();
                    let service = kanidm.create_pod_service(&pod_name);
                    kanidm.patch(ctx.clone(), service)
                })
                .collect::<Vec<_>>();
            try_join_all(futures)
        }
        _ => try_join_all(vec![]),
    };

    let sts_store = ctx.stores.stateful_set_store();
    let sts_to_delete = sts_store
        .state()
        .into_iter()
        .filter(|sts| {
            sts.metadata.labels.iter().any(|l| {
                l.get(CLUSTER_LABEL) == Some(&kanidm.name_any())
                    && match l.get(REPLICA_GROUP_LABEL) {
                        Some(sts_rg_name) => kanidm
                            .spec
                            .replica_groups
                            .iter()
                            .all(|rg| &rg.name != sts_rg_name),
                        None => false,
                    }
            })
        })
        .collect::<Vec<_>>();
    let sts_delete_future = sts_to_delete
        .iter()
        .map(|sts| kanidm.delete(ctx.clone(), sts.as_ref()))
        .collect::<TryJoinAll<_>>();

    let sts_futures = kanidm
        .spec
        .replica_groups
        .iter()
        .map(|rg| kanidm.patch(ctx.clone(), kanidm.create_statefulset(rg)))
        .collect::<TryJoinAll<_>>();
    let service_future = kanidm.patch(ctx.clone(), kanidm.create_service());
    let ingress_future = kanidm
        .create_ingress()
        .into_iter()
        .map(|ingress| kanidm.patch(ctx.clone(), ingress))
        .collect::<TryJoinAll<_>>();

    try_join!(
        sts_delete_future,
        admin_secret_future,
        services_per_pod_futures,
        replication_secret_future,
        sts_futures,
        service_future,
        ingress_future
    )?;
    Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
}

impl Kanidm {
    #[inline]
    fn generate_resource_labels(&self) -> BTreeMap<String, String> {
        LABELS
            .clone()
            .into_iter()
            .chain([
                (INSTANCE_LABEL.to_string(), self.name_any()),
                (CLUSTER_LABEL.to_string(), self.name_any()),
            ])
            .collect()
    }

    #[inline]
    fn get_tls_secret_name(&self) -> String {
        self.spec.tls_secret_name.clone().unwrap_or_else(|| {
            self.spec
                .ingress
                .as_ref()
                .and_then(|i| i.tls_secret_name.clone())
                .unwrap_or_else(|| format!("{}-tls", self.name_any()))
        })
    }

    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: Kanidm is namespaced scoped
        self.namespace().unwrap()
    }

    #[inline]
    fn is_replication_enabled(&self) -> bool {
        // safe unwrap: at least one replica group is required
        self.spec.replica_groups.len() > 1
            || self.spec.replica_groups.first().unwrap().replicas > 1
            || !self.spec.external_replication_nodes.is_empty()
    }

    async fn patch<K>(&self, ctx: Arc<Context<Kanidm>>, resource: K) -> Result<K>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        let name = resource.name_any();
        let namespace = self.get_namespace();
        trace!(
            msg = format!("patching {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let resource_api = Api::<K>::namespaced(ctx.client.clone(), &namespace);

        let result = resource_api
            .patch(
                &name,
                &PatchParams::apply(KANIDM_OPERATOR_NAME).force(),
                &Patch::Apply(&resource),
            )
            .await;
        match result {
            Ok(resource) => Ok(resource),
            Err(e) => match e {
                kube::Error::Api(ae) if ae.code == 422 => {
                    info!(
                        msg = format!(
                            "recreating {} because the update operation was not possible",
                            short_type_name::<K>().unwrap_or("Unknown")
                        ),
                        reason = ae.reason
                    );
                    trace!(msg = "operation was not posible because of 422", ?ae);
                    self.delete(ctx.clone(), &resource).await?;
                    ctx.metrics.reconcile_deploy_delete_create_inc();
                    resource_api
                        .patch(
                            &name,
                            &PatchParams::apply(KANIDM_OPERATOR_NAME).force(),
                            &Patch::Apply(&resource),
                        )
                        .await
                        .map_err(|e| {
                            Error::KubeError(
                                format!(
                                    "failed to re-try patch {} {namespace}/{name}",
                                    short_type_name::<K>().unwrap_or("Unknown")
                                ),
                                e,
                            )
                        })
                }
                _ => Err(Error::KubeError(
                    format!(
                        "failed to patch {} {namespace}/{name}",
                        short_type_name::<K>().unwrap_or("Unknown")
                    ),
                    e,
                )),
            },
        }
    }

    async fn delete<K>(&self, ctx: Arc<Context<Kanidm>>, resource: &K) -> Result<(), Error>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        let name = resource.name_any();
        let namespace = self.get_namespace();
        trace!(
            msg = format!("deleting {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let api = Api::<K>::namespaced(ctx.client.clone(), &namespace);
        api.delete(&name, &Default::default()).await.map_err(|e| {
            Error::KubeError(
                format!(
                    "failed to delete {} {namespace}/{name}",
                    short_type_name::<K>().unwrap_or("Unknown")
                ),
                e,
            )
        })?;
        Ok(())
    }

    async fn exec<I, T>(
        &self,
        ctx: Arc<Context<Kanidm>>,
        pod_name: &str,
        command: I,
    ) -> Result<Option<String>>
    where
        I: IntoIterator<Item = T> + Debug,
        T: Into<String>,
    {
        let namespace = &self.get_namespace();
        trace!(
            msg = "pod exec",
            resource.name = &pod_name,
            resource.namespace = &namespace,
            ?command
        );
        let pod = Api::<Pod>::namespaced(ctx.client.clone(), namespace);
        let attached = pod
            .exec(pod_name, command, &AttachParams::default().stderr(false))
            .await
            .map_err(|e| {
                Error::KubeError(format!("failed to exec pod {namespace}/{pod_name}"), e)
            })?;
        Ok(get_output(attached).await)
    }

    async fn exec_any<I, T>(&self, ctx: Arc<Context<Kanidm>>, command: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = T> + Debug,
        T: Into<String>,
    {
        // TODO: if replicas > 1 and replicas initialized, exec on pod available
        // safe unwrap: at least one replica group is required
        let sts_name = self.statefulset_name(&self.spec.replica_groups.first().unwrap().name);
        let pod_name = format!("{sts_name}-0");
        self.exec(ctx, &pod_name, command).await
    }
}

#[cfg(test)]
mod test {
    use super::{reconcile_kanidm, Kanidm};

    use crate::crd::KanidmStatus;
    use crate::reconcile::statefulset::StatefulSetExt;
    use k8s_openapi::api::core::v1::{Secret, Service};
    use k8s_openapi::api::networking::v1::Ingress;
    use kaniop_operator::controller::{Context, State, Stores};
    use kaniop_operator::error::Result;
    use kube::api::ObjectMeta;
    use kube::runtime::watcher;

    use std::sync::Arc;

    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::StatefulSet;
    use kube::runtime::reflector::store::Writer;
    use kube::{client::Body, Client, Resource, ResourceExt};
    use serde_json::json;

    impl Kanidm {
        /// A normal test kanidm with a given status
        pub fn test() -> Self {
            let mut e = Kanidm::new(
                "test",
                serde_json::from_value(json!({
                    "domain": "idm.example.com",
                    "tlsSecretName": "test-tls",
                    "replicaGroups": [
                        {
                            "name": "default",
                            "replicas": 1
                        }
                    ]
                }))
                .unwrap(),
            );
            e.meta_mut().namespace = Some("default".into());
            e
        }

        pub fn with_ingress(mut self) -> Self {
            self.spec.ingress = Some(serde_json::from_value(json!({})).unwrap());
            self
        }

        /// Modify kanidm replicas
        pub fn with_replicas(mut self, replicas: i32) -> Self {
            // TODO: manage replica_groups
            self.spec.replica_groups[0].replicas = replicas;
            self
        }

        /// Modify kanidm to set a deletion timestamp
        pub fn needs_delete(mut self) -> Self {
            use chrono::prelude::{DateTime, TimeZone, Utc};
            let now: DateTime<Utc> = Utc.with_ymd_and_hms(2017, 4, 2, 12, 50, 32).unwrap();
            use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
            self.meta_mut().deletion_timestamp = Some(Time(now));
            self
        }

        /// Modify a kanidm to have an expected status
        pub fn with_status(mut self, status: KanidmStatus) -> Self {
            self.status = Some(status);
            self
        }
    }

    // We wrap tower_test::mock::Handle
    type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;
    pub struct ApiServerVerifier(ApiServerHandle);

    /// Scenarios we test for in ApiServerVerifier
    pub enum Scenario {
        Create(Kanidm),
        CreateWithTwoReplicas(Kanidm),
        CreateWithIngress(Kanidm),
        CreateWithIngressWithTwoReplicas(Kanidm),
    }

    pub async fn timeout_after_1s(handle: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("timeout on mock apiserver")
            .expect("scenario succeeded")
    }

    impl ApiServerVerifier {
        /// Tests only get to run specific scenarios that has matching handlers
        ///
        /// This setup makes it easy to handle multiple requests by chaining handlers together.
        ///
        /// NB: If the controller is making more calls than we are handling in the scenario,
        /// you then typically see a `KubeError(Service(Closed(())))` from the reconciler.
        ///
        /// You should await the `JoinHandle` (with a timeout) from this function to ensure that the
        /// scenario runs to completion (i.e. all expected calls were responded to),
        /// using the timeout to catch missing api calls to Kubernetes.
        pub fn run(self, scenario: Scenario) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                // moving self => one scenario per test
                match scenario {
                    Scenario::Create(kanidm) => {
                        self.handle_kanidm_status_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                    }
                    Scenario::CreateWithTwoReplicas(kanidm) => {
                        self.handle_kanidm_status_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                    }
                    Scenario::CreateWithIngress(kanidm) => {
                        self.handle_kanidm_status_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_ingress_patch(kanidm.clone())
                            .await
                    }
                    Scenario::CreateWithIngressWithTwoReplicas(kanidm) => {
                        self.handle_kanidm_status_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_ingress_patch(kanidm.clone())
                            .await
                    }
                }
                .expect("scenario completed without errors");
            })
        }

        async fn handle_kanidm_status_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/kaniop.rs/v1beta1/namespaces/default/kanidms/{}/status?&force=true&fieldManager=kanidms.kaniop.rs",
                    kanidm.name_any()
                )
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let status: KanidmStatus = serde_json::from_value(json.get("status").unwrap().clone())
                .expect("valid kanidm status");
            let response = serde_json::to_vec(&status).unwrap();
            // pass through kanidm "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_statefulset_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            for rg in kanidm.spec.replica_groups.iter() {
                let (request, send) = self.0.next_request().await.expect("service not called");
                assert_eq!(request.method(), http::Method::PATCH);
                assert_eq!(
                    request.uri().to_string(),
                    format!(
                        "/apis/apps/v1/namespaces/default/statefulsets/{}?&force=true&fieldManager=kanidms.kaniop.rs",
                        kanidm.statefulset_name(&rg.name)
                    )
                );
                let req_body = request.into_body().collect_bytes().await.unwrap();
                let json: serde_json::Value =
                    serde_json::from_slice(&req_body).expect("patch object is json");
                let statefulset: StatefulSet =
                    serde_json::from_value(json).expect("valid statefulset");
                assert_eq!(
                    statefulset.clone().spec.unwrap().replicas.unwrap(),
                    rg.replicas
                );
                let response = serde_json::to_vec(&statefulset).unwrap();
                // pass through kanidm "patch accepted"
                send.send_response(Response::builder().body(Body::from(response)).unwrap());
            }

            Ok(self)
        }

        async fn handle_service_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/api/v1/namespaces/default/services/{}?&force=true&fieldManager=kanidms.kaniop.rs",
                    kanidm.name_any()
                )
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let service: Service = serde_json::from_value(json).expect("valid service");
            let response = serde_json::to_vec(&service).unwrap();
            // pass through kanidm "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_ingress_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/networking.k8s.io/v1/namespaces/default/ingresses/{}?&force=true&fieldManager=kanidms.kaniop.rs",
                    kanidm.name_any()
                )
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let ingress: Ingress = serde_json::from_value(json).expect("valid service");
            let response = serde_json::to_vec(&ingress).unwrap();
            // pass through kanidm "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }
    }

    pub fn get_test_context() -> (Arc<Context<Kanidm>>, ApiServerVerifier) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mock_client = Client::new(mock_service, "default");
        let mut secret_writer = Writer::default();
        let mock_tls_secret = Secret {
            metadata: ObjectMeta {
                name: Some("test-tls".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        secret_writer.apply_watcher_event(&watcher::Event::InitApply(mock_tls_secret));
        secret_writer.apply_watcher_event(&watcher::Event::InitDone);
        let stores = Stores::new(
            Some(Writer::default().as_reader()),
            Some(Writer::default().as_reader()),
            Some(Writer::default().as_reader()),
            Some(secret_writer.as_reader()),
        );
        let controller_id = "test";
        let state = State::new(Default::default(), &[controller_id]);
        let ctx = state.to_context(mock_client, controller_id, stores);
        (ctx, ApiServerVerifier(handle))
    }

    #[tokio::test]
    async fn kanidm_create() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test();
        let mocksrv = fakeserver.run(Scenario::Create(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn kanidm_create_with_two_replicas() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test().with_replicas(2);
        let mocksrv = fakeserver.run(Scenario::CreateWithTwoReplicas(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn kanidm_create_with_ingress() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test().with_ingress();
        let mocksrv = fakeserver.run(Scenario::CreateWithIngress(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn kanidm_create_with_ingress_with_two_replicas() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test().with_ingress().with_replicas(2);
        let mocksrv = fakeserver.run(Scenario::CreateWithIngressWithTwoReplicas(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
}
