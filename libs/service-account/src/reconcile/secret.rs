use crate::controller::CONTROLLER_ID;
use crate::crd::KanidmServiceAccount;

use kanidm_client::KanidmClient;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::rotation::{
    add_rotation_annotations as add_annotations, needs_rotation as rotation_needs_rotation,
};
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use kaniop_operator::crd::SecretRotation;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use k8s_openapi::api::core::v1::Secret;
use kube::ResourceExt;
use kube::api::{ObjectMeta, PartialObjectMeta, Resource};

static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        (NAME_LABEL.to_string(), "kanidm".to_string()),
        (
            MANAGED_BY_LABEL.to_string(),
            format!("kaniop-{CONTROLLER_ID}"),
        ),
    ])
});
pub const TOKEN_LABEL: &str = "apitoken.kaniop.rs/label";
pub const CREDENTIAL_LABEL: &str = "credential.kaniop.rs/account";

/// Add rotation annotations based on a SecretRotation config.
fn add_rotation_annotations(
    annotations: &mut BTreeMap<String, String>,
    rotation_config: Option<&SecretRotation>,
) {
    if let Some(config) = rotation_config {
        add_annotations(annotations, config.enabled, config.period_days);
    }
}

/// Check if a secret needs rotation based on its annotations.
pub fn needs_rotation(
    secret: &PartialObjectMeta<Secret>,
    rotation_config: Option<&SecretRotation>,
) -> bool {
    match rotation_config {
        Some(config) if config.enabled => {
            rotation_needs_rotation(secret, config.enabled, config.period_days)
        }
        _ => false,
    }
}

pub trait SecretExt {
    fn generate_token_secret_name(&self, token_label: &str) -> String;
    fn generate_token_secret(
        &self,
        token_label: &str,
        token: &str,
        secret_name: Option<&str>,
        rotation_config: Option<&SecretRotation>,
    ) -> Secret;
    fn credentials_secret_name(&self) -> String;

    async fn generate_credentials_secret(
        &self,
        kanidm_client: &KanidmClient,
        rotation_config: Option<&SecretRotation>,
    ) -> Result<Secret>;
}

impl SecretExt for KanidmServiceAccount {
    #[inline]
    fn generate_token_secret_name(&self, token_label: &str) -> String {
        format!("{}-{token_label}-api-token", self.name_any())
    }

    fn generate_token_secret(
        &self,
        token_label: &str,
        token: &str,
        secret_name: Option<&str>,
        rotation_config: Option<&SecretRotation>,
    ) -> Secret {
        let secret_name = match secret_name {
            Some(s) => s.to_string(),
            None => self.generate_token_secret_name(token_label),
        };
        let labels = LABELS
            .clone()
            .into_iter()
            .chain([
                (INSTANCE_LABEL.to_string(), self.name_any()),
                (TOKEN_LABEL.to_string(), token_label.to_string()),
            ])
            .collect();

        let mut annotations = BTreeMap::new();
        add_rotation_annotations(&mut annotations, rotation_config);

        Secret {
            metadata: ObjectMeta {
                name: Some(secret_name),
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
                [("token".to_string(), token.to_string())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            ..Secret::default()
        }
    }

    #[inline]
    fn credentials_secret_name(&self) -> String {
        format!("{}-kanidm-service-account-credentials", self.name_any())
    }

    async fn generate_credentials_secret(
        &self,
        kanidm_client: &KanidmClient,
        rotation_config: Option<&SecretRotation>,
    ) -> Result<Secret> {
        let name = &self.name_any();
        let credentials = kanidm_client
            .idm_service_account_generate_password(name)
            .await
            .map_err(|e| {
                Error::KanidmClientError(
                    format!(
                        "failed to generate credentials for {name} from {namespace}/{kanidm}",
                        namespace = self.kanidm_namespace(),
                        kanidm = self.kanidm_name(),
                    ),
                    Box::new(e),
                )
            })?;
        let labels = LABELS
            .clone()
            .into_iter()
            .chain([
                (INSTANCE_LABEL.to_string(), name.clone()),
                (CREDENTIAL_LABEL.to_string(), name.clone()),
            ])
            .collect();

        let mut annotations = BTreeMap::new();
        add_rotation_annotations(&mut annotations, rotation_config);

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(self.credentials_secret_name()),
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
                [("password".to_string(), credentials)]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            ..Secret::default()
        };

        Ok(secret)
    }
}
