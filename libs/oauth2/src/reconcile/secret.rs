use crate::controller::CONTROLLER_ID;
use crate::crd::KanidmOAuth2Client;

use kanidm_client::KanidmClient;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::rotation::add_rotation_annotations as add_annotations;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use kaniop_operator::crd::SecretRotation;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use k8s_openapi::api::core::v1::Secret;
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        (NAME_LABEL.to_string(), "kanidm".to_string()),
        (
            MANAGED_BY_LABEL.to_string(),
            format!("kaniop-{CONTROLLER_ID}"),
        ),
    ])
});

/// Add rotation annotations based on a SecretRotation config.
fn add_rotation_annotations(
    annotations: &mut BTreeMap<String, String>,
    rotation_config: Option<&SecretRotation>,
) {
    if let Some(config) = rotation_config {
        add_annotations(annotations, config.enabled, config.period_days);
    }
}

#[allow(async_fn_in_trait)]
pub trait SecretExt {
    fn secret_name(&self) -> String;
    async fn generate_secret(
        &self,
        kanidm_client: &KanidmClient,
        rotation_config: Option<&SecretRotation>,
    ) -> Result<Secret>;
}

impl SecretExt for KanidmOAuth2Client {
    #[inline]
    fn secret_name(&self) -> String {
        format!("{}-kanidm-oauth2-credentials", self.name_any())
    }

    async fn generate_secret(
        &self,
        kanidm_client: &KanidmClient,
        rotation_config: Option<&SecretRotation>,
    ) -> Result<Secret> {
        let name = &self.name_any();
        let client_secret = kanidm_client
            .idm_oauth2_rs_get_basic_secret(name)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to get basic secret for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?
            .ok_or_else(|| {
                Error::MissingData(format!(
                    "no basic secret for {name} in {namespace}/{kanidm}",
                    namespace = self.kanidm_namespace(),
                    kanidm = self.kanidm_name(),
                ))
            })?;
        let labels = LABELS
            .clone()
            .into_iter()
            .chain([(INSTANCE_LABEL.to_string(), name.clone())])
            .collect();

        let mut annotations = BTreeMap::new();
        add_rotation_annotations(&mut annotations, rotation_config);

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(self.secret_name()),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                labels: Some(labels),
                annotations: if annotations.is_empty() {
                    None
                } else {
                    Some(annotations)
                },
                ..ObjectMeta::default()
            },
            string_data: Some(
                [
                    ("CLIENT_ID".to_string(), name.clone()),
                    ("CLIENT_SECRET".to_string(), client_secret),
                ]
                .iter()
                .cloned()
                .collect(),
            ),
            ..Secret::default()
        };

        Ok(secret)
    }
}

impl KanidmOAuth2Client {}
