pub mod secret;
pub mod statefulset;

mod ingress;
mod service;
mod status;

use super::controller::{CONTROLLER_ID, context::Context};

use self::ingress::IngressExt;
use self::secret::SecretExt;
use self::service::ServiceExt;
use self::statefulset::{REPLICA_GROUP_LABEL, StatefulSetExt};
use self::status::StatusExt;

use crate::controller::{DEFAULT_RECONCILE_INTERVAL, INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use crate::error::{Error, Result};
use crate::kanidm::crd::{Kanidm, KanidmReplicaState, KanidmStatus};
use crate::telemetry;

use kaniop_k8s_util::client::get_output;
use kaniop_k8s_util::types::short_type_name;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

use futures::future::{TryJoinAll, join_all, try_join_all};
use futures::try_join;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use kube::api::{Api, AttachParams, Patch, PatchParams, Resource};
use kube::core::NamespaceResourceScope;
use kube::runtime::controller::Action;
use serde::{Deserialize, Serialize};
use status::{is_kanidm_available, is_kanidm_initialized};
use tracing::{Span, debug, field, info, instrument, trace};

pub const CLUSTER_LABEL: &str = "kanidm.kaniop.rs/cluster";
const KANIDM_OPERATOR_NAME: &str = "kanidms.kaniop.rs";

static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        (NAME_LABEL.to_string(), "kanidm".to_string()),
        (
            MANAGED_BY_LABEL.to_string(),
            format!("kaniop-{CONTROLLER_ID}"),
        ),
    ])
});

pub async fn reconcile_admins_secret(
    kanidm: Arc<Kanidm>,
    ctx: Arc<Context>,
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
    ctx: Arc<Context>,
    status: &Result<KanidmStatus>,
) -> Result<()> {
    if let Ok(s) = status {
        let secret_names = s
            .replica_statuses
            .iter()
            .map(|rs| kanidm.replica_secret_name(&rs.pod_name))
            .collect::<Vec<_>>();
        let deprecated_secrets = ctx
            .stores
            .secret_store
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
            let sts_api = Api::<StatefulSet>::namespaced(
                ctx.kaniop_ctx.client.clone(),
                &kanidm.get_namespace(),
            );
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

#[instrument(skip(ctx, kanidm))]
pub async fn reconcile_kanidm(kanidm: Arc<Kanidm>, ctx: Arc<Context>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx
        .kaniop_ctx
        .metrics
        .reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling Kanidm");

    let status = kanidm.update_status(ctx.clone()).await.map_err(|e| {
        debug!(msg = "failed to reconcile status", %e);
        ctx.kaniop_ctx.metrics.status_update_errors_inc();
        e
    });

    let admin_secret_future = reconcile_admins_secret(kanidm.clone(), ctx.clone(), &status);
    let replication_secret_future =
        reconcile_replication_secrets(kanidm.clone(), ctx.clone(), &status);

    let sts_to_delete = ctx
        .stores
        .stateful_set_store
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
        format!("{}-tls", self.name_any())
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

    async fn patch<K>(&self, ctx: Arc<Context>, obj: K) -> Result<K>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        let name = obj.name_any();
        let namespace = self.get_namespace();
        trace!(
            msg = format!("patching {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let resource_api = Api::<K>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);

        let result = resource_api
            .patch(
                &name,
                &PatchParams::apply(KANIDM_OPERATOR_NAME).force(),
                &Patch::Apply(&obj),
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
                    self.delete(ctx.clone(), &obj).await?;
                    ctx.kaniop_ctx.metrics.reconcile_deploy_delete_create_inc();
                    resource_api
                        .patch(
                            &name,
                            &PatchParams::apply(KANIDM_OPERATOR_NAME).force(),
                            &Patch::Apply(&obj),
                        )
                        .await
                        .map_err(|e| {
                            Error::KubeError(
                                format!(
                                    "failed to re-try patch {} {namespace}/{name}",
                                    short_type_name::<K>().unwrap_or("Unknown")
                                ),
                                Box::new(e),
                            )
                        })
                }
                _ => Err(Error::KubeError(
                    format!(
                        "failed to patch {} {namespace}/{name}",
                        short_type_name::<K>().unwrap_or("Unknown")
                    ),
                    Box::new(e),
                )),
            },
        }
    }

    async fn delete<K>(&self, ctx: Arc<Context>, obj: &K) -> Result<(), Error>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        let name = obj.name_any();
        let namespace = self.get_namespace();
        trace!(
            msg = format!("deleting {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let api = Api::<K>::namespaced(ctx.kaniop_ctx.client.clone(), &namespace);
        api.delete(&name, &Default::default()).await.map_err(|e| {
            Error::KubeError(
                format!(
                    "failed to delete {} {namespace}/{name}",
                    short_type_name::<K>().unwrap_or("Unknown")
                ),
                Box::new(e),
            )
        })?;
        Ok(())
    }

    async fn exec<I, T>(
        &self,
        ctx: Arc<Context>,
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
        let pod = Api::<Pod>::namespaced(ctx.kaniop_ctx.client.clone(), namespace);
        let attached = pod
            .exec(pod_name, command, &AttachParams::default().stderr(false))
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to exec pod {namespace}/{pod_name}"),
                    Box::new(e),
                )
            })?;
        Ok(get_output(attached).await)
    }

    async fn exec_any<I, T>(&self, ctx: Arc<Context>, command: I) -> Result<Option<String>>
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
    use super::statefulset::StatefulSetExt;
    use super::{Kanidm, reconcile_kanidm};

    use crate::controller::State;
    use crate::error::Result;
    use crate::kanidm::controller::context::{Context, Stores};
    use crate::kanidm::crd::KanidmStatus;
    use k8s_openapi::api::core::v1::Service;
    use k8s_openapi::api::networking::v1::Ingress;

    use std::sync::Arc;

    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::StatefulSet;
    use kube::runtime::reflector::store::Writer;
    use kube::{Client, Resource, ResourceExt, client::Body};
    use serde_json::json;

    impl Kanidm {
        /// A normal test kanidm with a given status
        pub fn test() -> Self {
            let mut e = Kanidm::new(
                "test",
                serde_json::from_value(json!({
                    "domain": "idm.example.com",
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

    pub fn get_test_context() -> (Arc<Context>, ApiServerVerifier) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mock_client = Client::new(mock_service, "default");
        let stores = Stores {
            stateful_set_store: Writer::default().as_reader(),
            service_store: Writer::default().as_reader(),
            ingress_store: Writer::default().as_reader(),
            secret_store: Writer::default().as_reader(),
        };
        let controller_id = "test";
        let state = State::new(
            Default::default(),
            &[controller_id],
            Writer::default().as_reader(),
            Writer::default().as_reader(),
        );
        let ctx = Arc::new(Context::new(
            state.to_context(mock_client, controller_id),
            stores,
        ));
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
