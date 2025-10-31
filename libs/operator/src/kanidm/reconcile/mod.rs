mod ingress;
pub mod secret;
mod service;
pub mod statefulset;
mod status;

use super::controller::{CONTROLLER_ID, context::Context};

use self::ingress::IngressExt;
use self::secret::SecretExt;
use self::service::ServiceExt;
use self::statefulset::StatefulSetExt;
use self::status::StatusExt;
use self::status::{is_kanidm_available, is_kanidm_initialized};

use crate::controller::context::KubeOperations;
use crate::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use crate::kanidm::crd::{Kanidm, KanidmReplicaState, KanidmStatus, KanidmUpgradeCheckResult};
use crate::kanidm::reconcile::statefulset::REPLICA_LABEL;
use crate::telemetry;

use kaniop_k8s_util::client::get_output;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::resources::get_image_tag;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

use futures::future::{TryJoinAll, join_all, try_join_all};
use futures::stream::{self, StreamExt};
use futures::try_join;
use k8s_openapi::NamespaceResourceScope;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use kube::Resource;
use kube::ResourceExt;
use kube::api::{Api, AttachParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use serde::{Deserialize, Serialize};
use tokio::time::Duration;
use tracing::{Span, debug, field, info, instrument, trace, warn};

pub const CLUSTER_LABEL: &str = "kanidm.kaniop.rs/cluster";
const KANIDM_OPERATOR_NAME: &str = "kanidms.kaniop.rs";
pub static KANIDM_FINALIZER: &str = "kanidms.kaniop.rs/finalizer";

const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

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
    status: &KanidmStatus,
) -> Result<()> {
    if is_kanidm_available(status.clone()) && !is_kanidm_initialized(status.clone()) {
        let admins_secret = kanidm.generate_admins_secret(ctx.clone()).await?;
        kanidm.patch(&ctx, admins_secret).await?;
    }
    Ok(())
}

pub async fn reconcile_replication_secrets(
    kanidm: Arc<Kanidm>,
    ctx: Arc<Context>,
    status: &KanidmStatus,
) -> Result<()> {
    let expected_secret_names = if kanidm.is_replication_enabled() {
        status
            .replica_statuses
            .iter()
            .map(|rs| kanidm.replica_secret_name(&rs.pod_name))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let deprecated_secrets = ctx
        .stores
        .secret_store
        .state()
        .into_iter()
        .filter(|secret| {
            secret.namespace() == kanidm.namespace()
                && kanidm.admins_secret_name() != secret.name_any()
                && !expected_secret_names.contains(&secret.name_any())
                && secret
                    .metadata
                    .labels
                    .as_ref()
                    .is_some_and(|l| l.get(CLUSTER_LABEL) == Some(&kanidm.name_any()))
        })
        .collect::<Vec<_>>();
    let secret_delete_future = deprecated_secrets
        .iter()
        .map(|secret| kanidm.delete(&ctx, secret.as_ref()))
        .collect::<TryJoinAll<_>>();
    try_join!(secret_delete_future)?;

    if kanidm.is_replication_enabled() {
        let pending_secrets = status
            .replica_statuses
            .iter()
            .filter(|rs| rs.state == KanidmReplicaState::Pending)
            .map(|rs| kanidm.generate_replica_secret(ctx.clone(), &rs.pod_name))
            .collect::<TryJoinAll<_>>();

        let expiring_secrets = status
            .replica_statuses
            .iter()
            .filter(|rs| match rs.state {
                KanidmReplicaState::CertificateExpiring
                | KanidmReplicaState::CertificateHostInvalid => true,
                KanidmReplicaState::Ready | KanidmReplicaState::Pending => false,
            })
            .map(|rs| kanidm.update_replica_secret(ctx.clone(), &rs.pod_name))
            .collect::<TryJoinAll<_>>();

        let (pending_results, expiring_results) =
            futures::try_join!(pending_secrets, expiring_secrets)?;

        let secrets = pending_results
            .into_iter()
            .chain(expiring_results)
            .collect::<Vec<_>>();
        let secret_futures = secrets
            .into_iter()
            .map(|secret| kanidm.patch(&ctx, secret))
            .collect::<Vec<_>>();
        try_join_all(secret_futures).await?;

        // If there are any replicas in pending or cert expiring state, trigger a restart
        // of all statefulsets to pickup new certs
        let has_pending_or_expiring = status
            .replica_statuses
            .iter()
            .any(|rs| rs.state != KanidmReplicaState::Ready);

        if has_pending_or_expiring {
            let sts_api = Api::<StatefulSet>::namespaced(
                ctx.kaniop_ctx.client.clone(),
                &kanidm.get_namespace(),
            );

            // Check if any replica group has only 1 replica
            let has_single_replica = kanidm.spec.replica_groups.iter().any(|rg| rg.replicas == 1);

            let restart_futures = status
                .replica_statuses
                .iter()
                .map(|rs| sts_api.restart(&rs.statefulset_name));

            if has_single_replica {
                // Restart sequentially to avoid downtime
                let _ignore_errors = stream::iter(restart_futures)
                    .then(|f| f)
                    .collect::<Vec<_>>()
                    .await;
            } else {
                // Restart concurrently for replica groups with multiple replicas
                let _ignore_errors = join_all(restart_futures).await;
            }
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
    })?;
    let kanidm_api: Api<Kanidm> =
        Api::namespaced(ctx.kaniop_ctx.client.clone(), &kanidm.get_namespace());
    finalizer(&kanidm_api, KANIDM_FINALIZER, kanidm, |event| async {
        match event {
            Finalizer::Apply(kanidm) => reconcile(kanidm, ctx, status).await,
            Finalizer::Cleanup(kanidm) => cleanup(kanidm, ctx).await,
        }
    })
    .await
    .map_err(|e| {
        Error::FinalizerError(
            "failed on kanidm account finalizer".to_string(),
            Box::new(e),
        )
    })
}

async fn reconcile(kanidm: Arc<Kanidm>, ctx: Arc<Context>, status: KanidmStatus) -> Result<Action> {
    let admin_secret_future = reconcile_admins_secret(kanidm.clone(), ctx.clone(), &status);
    let replication_secret_futures =
        reconcile_replication_secrets(kanidm.clone(), ctx.clone(), &status);

    let sts_to_delete = {
        let expected_sts_names = kanidm
            .spec
            .replica_groups
            .iter()
            .map(|rg| kanidm.statefulset_name(&rg.name))
            .collect::<Vec<_>>();
        ctx.stores
            .stateful_set_store
            .state()
            .into_iter()
            .filter(|sts| {
                sts.namespace() == kanidm.namespace()
                    && sts
                        .metadata
                        .labels
                        .as_ref()
                        .is_some_and(|l| l.get(CLUSTER_LABEL) == Some(&kanidm.name_any()))
                    && !expected_sts_names.contains(&sts.name_any())
            })
            .collect::<Vec<_>>()
    };
    let sts_delete_futures = sts_to_delete
        .iter()
        .map(|sts| kanidm.delete(&ctx, sts.as_ref()))
        .collect::<TryJoinAll<_>>();

    let sts_futures = match kanidm.is_updatable(&status) {
        true => kanidm
            .spec
            .replica_groups
            .iter()
            .map(|rg| kanidm.patch(&ctx, kanidm.create_statefulset(rg, &ctx)))
            .collect::<TryJoinAll<_>>(),
        false => {
            let _ignore_error = ctx
                .kaniop_ctx
                .recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "UpgradeBlocked".to_string(),
                        note: Some(
                            "Version change blocked: upgrade pre-check failed. Override with `.spec.disableUpgradeCheck: true` or update the resource to retry.".to_string(),
                        ),
                        action: "ReconcileStatefulSet".to_string(),
                        secondary: None,
                    },
                    &kanidm.object_ref(&()),
                )
                .await
                .map_err(|e| {
                    warn!(msg = "failed to publish KanidmError event", %e);
                    Error::KubeError("failed to publish event".to_string(), Box::new(e))
                });
            try_join_all([].into_iter())
        }
    };
    let service_future = kanidm.patch(&ctx, kanidm.create_service());

    let deprecated_rg_svcs = {
        let expected_rg_svcs_names = kanidm
            .spec
            .replica_groups
            .iter()
            .filter(|rg| rg.services.is_some())
            .flat_map(|rg| (0..rg.replicas).map(|i| kanidm.replica_group_service_name(&rg.name, i)))
            .collect::<Vec<_>>();
        ctx.stores
            .service_store
            .state()
            .into_iter()
            .filter(|svc| {
                let labels = svc.metadata.labels.as_ref();
                svc.namespace() == kanidm.namespace()
                    && !expected_rg_svcs_names.contains(&svc.name_any())
                    && labels.is_some_and(|l| {
                        l.contains_key(REPLICA_LABEL)
                            && l.get(CLUSTER_LABEL) == Some(&kanidm.name_any())
                    })
            })
            .collect::<Vec<_>>()
    };
    let rg_svcs_delete_futures = deprecated_rg_svcs
        .iter()
        .map(|svc| kanidm.delete(&ctx, svc.as_ref()))
        .collect::<TryJoinAll<_>>();

    let rg_services_futures = kanidm
        .spec
        .replica_groups
        .iter()
        .filter(|rg| rg.services.is_some())
        .flat_map(|rg| {
            (0..rg.replicas)
                .map(|i| kanidm.patch(&ctx, kanidm.create_replica_group_service(&rg.name, i)))
        })
        .collect::<TryJoinAll<_>>();

    let deprecated_ingresses = {
        let names = [
            kanidm.spec.ingress.as_ref().map(|_| kanidm.name_any()),
            kanidm.generate_region_ingress_name(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        ctx.stores
            .ingress_store
            .state()
            .into_iter()
            .filter(|ing| {
                ing.namespace() == kanidm.namespace()
                    && !names.contains(&ing.name_any())
                    && ing.metadata.labels == Some(kanidm.generate_labels())
            })
            .collect::<Vec<_>>()
    };
    let ingress_delete_futures = deprecated_ingresses
        .iter()
        .map(|ing| kanidm.delete(&ctx, ing.as_ref()))
        .collect::<TryJoinAll<_>>();

    let ingress_futures = kanidm
        .create_ingress()
        .into_iter()
        .chain(kanidm.create_region_ingress())
        .map(|ingress| kanidm.patch(&ctx, ingress))
        .collect::<TryJoinAll<_>>();

    try_join!(
        sts_delete_futures,
        admin_secret_future,
        replication_secret_futures,
        sts_futures,
        service_future,
        rg_svcs_delete_futures,
        rg_services_futures,
        ingress_delete_futures,
        ingress_futures
    )?;
    Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
}

async fn cleanup(kanidm: Arc<Kanidm>, ctx: Arc<Context>) -> Result<Action> {
    debug!(msg = "cleanup");
    ctx.kaniop_ctx.release_kanidm_clients(&kanidm).await;
    Ok(Action::requeue(DEFAULT_RECONCILE_INTERVAL))
}

impl Kanidm {
    // Convenience methods that handle context and operator name
    pub async fn delete<K>(&self, ctx: &Context, resource: &K) -> Result<()>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        self.kube_delete(
            ctx.kaniop_ctx.client.clone(),
            &ctx.kaniop_ctx.metrics,
            resource,
        )
        .await
    }

    pub async fn patch<K>(&self, ctx: &Context, resource: K) -> Result<K>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        self.kube_patch(
            ctx.kaniop_ctx.client.clone(),
            &ctx.kaniop_ctx.metrics,
            resource,
            KANIDM_OPERATOR_NAME,
        )
        .await
    }

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
    fn generate_labels(&self) -> BTreeMap<String, String> {
        self.generate_resource_labels()
            .clone()
            .into_iter()
            .chain(self.labels().clone())
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

    #[inline]
    fn is_updatable(&self, status: &KanidmStatus) -> bool {
        status
            .version
            .as_ref()
            .map(|v| match v.upgrade_check_result {
                KanidmUpgradeCheckResult::Passed => true,
                KanidmUpgradeCheckResult::Failed => {
                    // TODO: check image upgrade is at least a minor version bump
                    v.image_tag == get_image_tag(&self.spec.image).unwrap_or_default()
                }
            })
            .unwrap_or(true)
    }

    async fn exec<I, T>(&self, ctx: Arc<Context>, pod_name: &str, command: I) -> Result<String>
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
            .exec(pod_name, command, &AttachParams::default())
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to exec pod {namespace}/{pod_name}"),
                    Box::new(e),
                )
            })?;
        get_output(attached).await
    }

    async fn exec_any<I, T>(&self, ctx: Arc<Context>, command: I) -> Result<String>
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
    use crate::kanidm::controller::context::{Context, Stores};
    use crate::kanidm::crd::KanidmStatus;

    use kaniop_k8s_util::error::Result;

    use std::sync::Arc;

    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::StatefulSet;
    use k8s_openapi::api::core::v1::Service;
    use k8s_openapi::api::networking::v1::Ingress;
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
            // Pre-populate finalizer so reconcile callback runs immediately
            // (skips the "add finalizer + wait for next reconcile" step)
            e.meta_mut().finalizers = Some(vec![super::KANIDM_FINALIZER.to_string()]);
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
            let mut kanidm = Kanidm::test();
            kanidm.status = Some(status.clone());
            let response = serde_json::to_vec(&kanidm).unwrap();
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
