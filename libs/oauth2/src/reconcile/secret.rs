use crate::controller::{CONTROLLER_ID, Context};
use crate::crd::{KanidmOAuth2Client, SecretKeyAliases};
use crate::reconcile::OAUTH2_OPERATOR_NAME;

use kanidm_client::KanidmClient;
use kanidm_proto::constants::ATTR_UUID;
use kanidm_proto::v1::Entry;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::rotation::add_rotation_annotations as add_annotations;
use kaniop_k8s_util::types::get_first_cloned;
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
        existing_rotation_annotations: Option<&BTreeMap<String, String>>,
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
        existing_rotation_annotations: Option<&BTreeMap<String, String>>,
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

        // The current CR controls the rotation policy. A repair must not change the
        // last successful rotation timestamp or invent one for an existing Secret.
        add_rotation_annotations(&mut annotations, rotation_config);
        if let Some(existing) = existing_rotation_annotations {
            let key = kaniop_k8s_util::rotation::ROTATION_LAST_TIME_ANNOTATION;
            if let Some(value) = existing.get(key) {
                annotations.insert(key.to_string(), value.clone());
            } else {
                annotations.remove(key);
            }
        }

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

impl KanidmOAuth2Client {
    /// Compare the entry identity and credential on every other local replica.
    /// No credential values are included in errors, logs, status, or events.
    pub(super) async fn credential_converged(
        &self,
        kanidm_client: &KanidmClient,
        ctx: &Context,
        entry: &Entry,
        credential: &str,
    ) -> Result<bool> {
        let uuid = get_first_cloned(entry, ATTR_UUID);
        let expected_uuid = uuid.as_deref();
        let name = self.kanidm_entity_name();
        let name = name.as_str();
        ctx.kaniop_ctx
            .verify_other_local_replicas(self, kanidm_client, |client| async move {
                let Some(uuid) = expected_uuid else {
                    return Ok(false);
                };
                let Some(replica_entry) = client.idm_oauth2_rs_get(name).await? else {
                    return Ok(false);
                };
                if get_first_cloned(&replica_entry, ATTR_UUID).as_deref() != Some(uuid) {
                    return Ok(false);
                }
                Ok(client.idm_oauth2_rs_get_basic_secret(name).await?.as_deref()
                    == Some(credential))
            })
            .await
    }

    /// Check the exact credential about to be published, not an earlier status
    /// snapshot. Re-read the selected writer before checking the other replicas.
    pub(super) async fn secret_converged(
        &self,
        kanidm_client: &KanidmClient,
        ctx: &Context,
        secret: &Secret,
    ) -> Result<bool> {
        let name = self.kanidm_entity_name();
        let Some(credential) = secret
            .data
            .as_ref()
            .and_then(|data| data.get("CLIENT_SECRET"))
            .and_then(|value| std::str::from_utf8(&value.0).ok())
        else {
            return Ok(false);
        };
        let Ok(Some(entry)) = kanidm_client.idm_oauth2_rs_get(&name).await else {
            return Ok(false);
        };
        let Ok(Some(current)) = kanidm_client.idm_oauth2_rs_get_basic_secret(&name).await else {
            return Ok(false);
        };
        if current != credential {
            return Ok(false);
        }
        self.credential_converged(kanidm_client, ctx, &entry, credential)
            .await
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
