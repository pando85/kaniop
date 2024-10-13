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

use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{Container, ContainerPort, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, ObjectMeta, Patch, PatchParams, Resource};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::ResourceExt;
use tokio::time::Duration;
use tracing::{debug, field, info, instrument, Span};

static KANIDM_CRD_NAME: &str = "kanidms.kaniop.rs";
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
    kanidm.patch(ctx).await?;
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

    async fn patch(&self, ctx: Arc<Context>) -> Result<StatefulSet, Error> {
        let namespace = self.get_namespace();
        let statefulset_api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &namespace);
        let owner_references = self.controller_owner_ref(&()).map(|oref| vec![oref]);

        let name = self.name_any();
        let labels: BTreeMap<String, String> = self
            .labels()
            .iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .chain([
                ("app".to_owned(), name.clone()),
                ("app.kubernetes.io/name".to_owned(), "kanidm".to_owned()),
                (
                    "app.kubernetes.io/managed-by".to_owned(),
                    "kaniop".to_owned(),
                ),
            ])
            .collect();

        ctx.metrics
            .spec_replicas_set(&namespace, &name, self.spec.replicas);
        let statefulset = StatefulSet {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                namespace: Some(namespace),
                labels: Some(labels.clone()),
                owner_references,
                ..ObjectMeta::default()
            },
            spec: Some(StatefulSetSpec {
                replicas: Some(self.spec.replicas),
                selector: LabelSelector {
                    match_expressions: None,
                    match_labels: Some(labels.clone()),
                },
                template: PodTemplateSpec {
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            name: self.name_any(),
                            image: Some("inanimate/echo-server:latest".to_owned()),
                            ports: Some(vec![ContainerPort {
                                container_port: 8080,
                                ..ContainerPort::default()
                            }]),
                            ..Container::default()
                        }],
                        ..PodSpec::default()
                    }),
                    metadata: Some(ObjectMeta {
                        labels: Some(labels),
                        ..ObjectMeta::default()
                    }),
                },
                ..StatefulSetSpec::default()
            }),
            ..StatefulSet::default()
        };

        let result = statefulset_api
            .patch(
                &self.name_any(),
                &PatchParams::apply(KANIDM_CRD_NAME).force(),
                &Patch::Apply(&statefulset),
            )
            .await;
        match result {
            Ok(statefulset) => Ok(statefulset),
            Err(e) => {
                match e {
                    kube::Error::Api(ae) if ae.code == 422 => {
                        info!(msg = "recreating StatefulSet because the update operation wasn't possible", reason=ae.reason);
                        self.delete_statefulset(ctx.client.clone()).await?;
                        ctx.metrics.reconcile_deploy_delete_create_inc();
                        statefulset_api
                            .patch(
                                &self.name_any(),
                                &PatchParams::apply(KANIDM_CRD_NAME).force(),
                                &Patch::Apply(&statefulset),
                            )
                            .await
                            .map_err(Error::KubeError)
                    }
                    _ => Err(Error::KubeError(e)),
                }
            }
        }
    }

    async fn delete_statefulset(&self, client: Client) -> Result<(), Error> {
        let statefulset_api = Api::<StatefulSet>::namespaced(client, &self.get_namespace());
        statefulset_api
            .delete(&self.name_any(), &Default::default())
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{reconcile_kanidm, Kanidm};

    use crate::crd::KanidmStatus;
    use crate::test::{get_test_context, timeout_after_1s, Scenario};

    use std::sync::Arc;

    #[tokio::test]
    async fn kanidm_create() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test(None);
        let mocksrv = fakeserver.run(Scenario::KanidmPatch(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn kanidm_causes_status_patch() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test(Some(KanidmStatus::default()));
        let mocksrv = fakeserver.run(Scenario::KanidmPatch(kanidm.clone()));
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn kanidm_with_replicas_causes_patch() {
        let (testctx, fakeserver) = get_test_context();
        let kanidm = Kanidm::test(Some(KanidmStatus::default())).change_replicas(3);
        let scenario = Scenario::KanidmPatch(kanidm.clone());
        let mocksrv = fakeserver.run(scenario);
        reconcile_kanidm(Arc::new(kanidm), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
}
