mod ingress;
mod service;
mod statefulset;
mod status;

use crate::crd::Kanidm;
// TODO: clean
#[allow(unused_imports)]
use crate::reconcile::ingress::IngressExt;
#[allow(unused_imports)]
use crate::reconcile::service::ServiceExt;
#[allow(unused_imports)]
use crate::reconcile::statefulset::StatefulSetExt;
use crate::reconcile::status::StatusExt;

use kaniop_operator::controller::Context;
use kaniop_operator::error::{Error, Result};
use kaniop_operator::telemetry;

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use futures::future::TryJoinAll;
use futures::try_join;
use kube::api::{Api, Patch, PatchParams, Resource};
use kube::core::NamespaceResourceScope;
use kube::runtime::controller::Action;
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use tokio::time::Duration;
use tracing::{debug, field, info, instrument, trace, Span};

const KANIDM_MAX_REPLICAS: i32 = 2;
static KANIOP_OPERATOR_NAME: &str = "kanidms.kaniop.rs";
static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "kanidm".to_string()),
        (
            "app.kubernetes.io/managed-by".to_string(),
            "kaniop".to_string(),
        ),
    ])
});

#[instrument(skip(ctx, kanidm))]
pub async fn reconcile_kanidm(kanidm: Arc<Kanidm>, ctx: Arc<Context>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling Kanidm");

    let _ignore_errors = kanidm.update_status(ctx.clone()).await.map_err(|e| {
        debug!(msg = "failed to reconcile status", %e);
        ctx.metrics.status_update_errors_inc();
    });

    // TODO: improve this. Every time we ensure that there are no more sts than replicas
    let sts_delete_futures = (kanidm.spec.replicas..KANIDM_MAX_REPLICAS)
        .map(|i| {
            let statefulset = kanidm.get_statefulset(&i);
            kanidm.delete(ctx.clone(), statefulset)
        })
        .collect::<TryJoinAll<_>>();

    let _ignore_errors = try_join!(sts_delete_futures).map_err(|e| {
        debug!(msg = "failed delete sts", %e);
    });

    let sts_futures = (0..kanidm.spec.replicas)
        .map(|i| {
            let statefulset = kanidm.get_statefulset(&i);
            kanidm.patch(ctx.clone(), statefulset)
        })
        .collect::<TryJoinAll<_>>();
    let service_future = kanidm.patch(ctx.clone(), kanidm.get_service());
    let ingress_future = kanidm
        .get_ingress()
        .into_iter()
        .map(|ingress| kanidm.patch(ctx.clone(), ingress))
        .collect::<TryJoinAll<_>>();

    try_join!(sts_futures, service_future, ingress_future)?;
    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}

impl Kanidm {
    #[inline]
    fn get_labels(&self) -> BTreeMap<String, String> {
        LABELS
            .clone()
            .into_iter()
            .chain([("app.kubernetes.io/instance".to_string(), self.name_any())])
            .collect()
    }

    #[inline]
    fn get_secret_name(&self) -> String {
        format!("{}-tls", self.name_any())
    }

    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: Kanidm is namespaced scoped
        self.namespace().unwrap()
    }

    async fn patch<K>(&self, ctx: Arc<Context>, resource: K) -> Result<K, Error>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        trace!(
            msg = "patching resource",
            resource.name = &resource.name_any(),
            resource.namespace = &resource.namespace().unwrap()
        );
        let namespace = self.get_namespace();
        let resource_api = Api::<K>::namespaced(ctx.client.clone(), &namespace);

        let result = resource_api
            .patch(
                &resource.name_any(),
                &PatchParams::apply(KANIOP_OPERATOR_NAME).force(),
                &Patch::Apply(&resource),
            )
            .await;
        match result {
            Ok(resource) => Ok(resource),
            Err(e) => match e {
                kube::Error::Api(ae) if ae.code == 422 => {
                    info!(
                        msg = format!(
                            "recreating {} because the update operation wasn't possible",
                            std::any::type_name::<K>()
                        ),
                        reason = ae.reason
                    );
                    self.delete(ctx.clone(), resource.clone()).await?;
                    ctx.metrics.reconcile_deploy_delete_create_inc();
                    resource_api
                        .patch(
                            &resource.name_any(),
                            &PatchParams::apply(KANIOP_OPERATOR_NAME).force(),
                            &Patch::Apply(&resource),
                        )
                        .await
                        .map_err(Error::KubeError)
                }
                _ => Err(Error::KubeError(e)),
            },
        }
    }

    async fn delete<K>(&self, ctx: Arc<Context>, resource: K) -> Result<(), Error>
    where
        K: Resource<Scope = NamespaceResourceScope>
            + Serialize
            + Clone
            + std::fmt::Debug
            + for<'de> Deserialize<'de>,
        <K as kube::Resource>::DynamicType: Default,
        <K as Resource>::Scope: std::marker::Sized,
    {
        trace!(
            msg = "deleting resource",
            resource.name = &resource.name_any(),
            resource.namespace = &resource.namespace().unwrap()
        );
        let api = Api::<K>::namespaced(ctx.client.clone(), &self.get_namespace());
        api.delete(&resource.name_any(), &Default::default())
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{reconcile_kanidm, Kanidm};

    use crate::crd::KanidmStatus;
    use k8s_openapi::api::core::v1::Service;
    use k8s_openapi::api::networking::v1::Ingress;
    use kaniop_operator::controller::{Context, Stores};
    use kaniop_operator::error::Result;

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
            self.spec.replicas = replicas;
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
                        self.handle_statefulset_delete_not_found(format!("{}-1", kanidm.name_any()))
                            .await
                            .unwrap()
                            .handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                    }
                    Scenario::CreateWithTwoReplicas(kanidm) => {
                        self.handle_statefulset_patch(kanidm.clone())
                            .await
                            .unwrap()
                            .handle_service_patch(kanidm.clone())
                            .await
                    }
                    Scenario::CreateWithIngress(kanidm) => {
                        self.handle_statefulset_delete_not_found(format!("{}-1", kanidm.name_any()))
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
                        self.handle_statefulset_patch(kanidm.clone())
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

        async fn handle_statefulset_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            for i in 0..kanidm.spec.replicas {
                let (request, send) = self.0.next_request().await.expect("service not called");
                assert_eq!(request.method(), http::Method::PATCH);
                assert_eq!(
                    request.uri().to_string(),
                    format!(
                        "/apis/apps/v1/namespaces/default/statefulsets/{}-{i}?&force=true&fieldManager=kanidms.kaniop.rs",
                        kanidm.name_any()
                    )
                );
                let req_body = request.into_body().collect_bytes().await.unwrap();
                let json: serde_json::Value =
                    serde_json::from_slice(&req_body).expect("patch object is json");
                let statefulset: StatefulSet =
                    serde_json::from_value(json).expect("valid statefulset");
                assert_eq!(
                    statefulset.clone().spec.unwrap().replicas.unwrap(),
                    1,
                    "statefulset replicas equal to 1"
                );
                let response = serde_json::to_vec(&statefulset).unwrap();
                // pass through kanidm "patch accepted"
                send.send_response(Response::builder().body(Body::from(response)).unwrap());
            }

            Ok(self)
        }

        async fn handle_statefulset_delete_not_found(mut self, name: String) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::DELETE);
            assert_eq!(
                request.uri().to_string(),
                format!("/apis/apps/v1/namespaces/default/statefulsets/{}?", name)
            );
            send.send_response(Response::builder().status(404).body(Body::empty()).unwrap());
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
        let stores = Stores::new(
            Some(Writer::default().as_reader()),
            Some(Writer::default().as_reader()),
            Some(Writer::default().as_reader()),
        );
        let ctx = Context {
            client: mock_client,
            metrics: Arc::default(),
            stores: Arc::new(stores),
        };
        (Arc::new(ctx), ApiServerVerifier(handle))
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
