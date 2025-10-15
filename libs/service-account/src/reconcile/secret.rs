use crate::controller::CONTROLLER_ID;
use crate::crd::KanidmServiceAccount;

use kaniop_operator::controller::{INSTANCE_LABEL, MANAGED_BY_LABEL, NAME_LABEL};

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
pub const TOKEN_LABEL: &str = "apitoken.kaniop.rs/label";

pub trait SecretExt {
    fn generate_token_secret_name(&self, token_label: &str) -> String;
    fn generate_token_secret(
        &self,
        token_label: &str,
        token: &str,
        secret_name: Option<&str>,
    ) -> Secret;
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
        Secret {
            metadata: ObjectMeta {
                name: Some(secret_name),
                namespace: Some(self.namespace().unwrap()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                labels: Some(labels),
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
}
