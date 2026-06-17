use crate::controller::CONTROLLER_ID;
use crate::crd::{KanidmOAuth2Client, SecretKeyAliases};
use crate::reconcile::OAUTH2_OPERATOR_NAME;

use kanidm_client::KanidmClient;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::rotation::add_rotation_annotations as add_annotations;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use kaniop_operator::crd::{MetadataTemplate, SecretRotation};
use kaniop_operator::object_meta_template::ObjectMetaTemplateExt;
use kube::ResourceExt;

use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{ObjectMeta, Resource};

use std::collections::BTreeMap;
use std::sync::LazyLock;

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
        secret_key_aliases: Option<&SecretKeyAliases>,
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
        secret_key_aliases: Option<&SecretKeyAliases>,
    ) -> Result<Secret> {
        let name = &self.kanidm_entity_name();
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

        let mut data: BTreeMap<String, ByteString> = BTreeMap::from([
            (
                "CLIENT_ID".to_string(),
                ByteString(name.clone().into_bytes()),
            ),
            (
                "CLIENT_SECRET".to_string(),
                ByteString(client_secret.clone().into_bytes()),
            ),
        ]);

        if let Some(aliases) = secret_key_aliases {
            let (client_id_aliases, client_secret_aliases) = aliases.collect_aliases();
            for alias in client_id_aliases {
                data.insert(alias, ByteString(name.clone().into_bytes()));
            }
            for alias in client_secret_aliases {
                data.insert(alias, ByteString(client_secret.clone().into_bytes()));
            }
        }

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
            data: Some(data),
            ..Secret::default()
        };

        Ok(secret)
    }
}

impl ObjectMetaTemplateExt<Secret> for KanidmOAuth2Client {
    const OPERATOR_NAME: &'static str = OAUTH2_OPERATOR_NAME;
    fn managed_object_name(&self) -> String {
        self.secret_name()
    }
    fn metadata_template(&self) -> Option<&MetadataTemplate> {
        self.spec.secret_template.as_ref()
    }
}
