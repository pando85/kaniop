use crate::controller::{Context, CONTROLLER_ID};
use crate::crd::KanidmOAuth2Client;

use kanidm_client::KanidmClient;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};
use kaniop_operator::error::{Error, Result};

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use k8s_openapi::api::core::v1::Secret;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

// decode with `basenc --base64url -d | openssl x509 -noout -text -inform DER`
pub const REPLICA_SECRET_KEY: &str = "tls.der.b64url";

const ADMIN_USER: &str = "admin";
const IDM_ADMIN_USER: &str = "idm_admin";

static LABELS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    BTreeMap::from([
        (NAME_LABEL.to_string(), "kanidm".to_string()),
        (
            MANAGED_BY_LABEL.to_string(),
            format!("kaniop-{CONTROLLER_ID}"),
        ),
    ])
});

#[allow(async_fn_in_trait)]
pub trait SecretExt {
    fn secret_name(&self) -> String;
    async fn generate_secret(&self, kanidm_client: &KanidmClient) -> Result<Secret>;
}

impl SecretExt for KanidmOAuth2Client {
    #[inline]
    fn secret_name(&self) -> String {
        format!("{}-kanidm-oauth2-credentials", self.name_any())
    }

    async fn generate_secret(&self, kanidm_client: &KanidmClient) -> Result<Secret> {
        let name = self.secret_name();
        let client_secret = kanidm_client
            .idm_oauth2_rs_get_basic_secret(&name)
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
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                labels: Some(labels),
                ..ObjectMeta::default()
            },
            string_data: Some(
                [
                    ("CLIENT_ID".to_string(), name),
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
