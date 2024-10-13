pub mod controller;
#[rustfmt::skip]
pub mod crd;
pub mod reconcile;

#[cfg(test)]
mod test {
    use crate::crd::{Kanidm, KanidmSpec, KanidmStatus};

    use kaniop_operator::controller::{Context, Stores};
    use kaniop_operator::error::Result;

    use std::sync::Arc;

    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::StatefulSet;
    use kube::runtime::reflector::store::Writer;
    use kube::{client::Body, Client, Resource, ResourceExt};

    impl Kanidm {
        /// A normal test kanidm with a given status
        pub fn test(status: Option<KanidmStatus>) -> Self {
            let mut e = Kanidm::new(
                "test",
                KanidmSpec {
                    domain: "idm.example.com".to_string(),
                    replicas: 1,
                    ..Default::default()
                },
            );
            e.meta_mut().namespace = Some("default".into());
            e.status = status;
            e
        }

        /// Modify kanidm replicas
        pub fn change_replicas(mut self, replicas: i32) -> Self {
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
        /// objects changes will cause a patch
        KanidmPatch(Kanidm),
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
                    Scenario::KanidmPatch(kanidm) => self.handle_kanidm_patch(kanidm.clone()).await,
                }
                .expect("scenario completed without errors");
            })
        }

        async fn handle_kanidm_patch(mut self, kanidm: Kanidm) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/apps/v1/namespaces/default/statefulsets/{}?&force=true&fieldManager=kanidms.kaniop.rs",
                    kanidm.name_any()
                )
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let statefulset: StatefulSet = serde_json::from_value(json).expect("valid statefulset");
            assert_eq!(
                statefulset.clone().spec.unwrap().replicas.unwrap(),
                kanidm.spec.replicas,
                "statefulset replicas equal to kanidm spec replicas"
            );
            let response = serde_json::to_vec(&statefulset).unwrap();
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
}
