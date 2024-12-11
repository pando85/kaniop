use crate::error::{Error, Result};

use kanidm_client::{KanidmClient, KanidmClientBuilder};

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Secret;
use kube::api::Api;
use kube::client::Client;
use serde::Serialize;
use serde_plain::derive_display_from_serialize;
use tracing::{debug, trace};

pub trait KanidmResource {
    fn kanidm_name(&self) -> String;
    fn kanidm_namespace(&self) -> String;
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum KanidmUser {
    IdmAdmin,
    Admin,
}

derive_display_from_serialize!(KanidmUser);

#[derive(Default)]
pub struct KanidmClients(HashMap<KanidmKey, Arc<KanidmClient>>);

impl KanidmClients {
    pub fn get(&self, key: &KanidmKey) -> Option<&Arc<KanidmClient>> {
        self.0.get(key)
    }

    pub fn insert(
        &mut self,
        key: KanidmKey,
        client: Arc<KanidmClient>,
    ) -> Option<Arc<KanidmClient>> {
        self.0.insert(key, client)
    }

    pub async fn create_client(
        namespace: &str,
        name: &str,
        user: KanidmUser,
        k_client: Client,
    ) -> Result<Arc<KanidmClient>> {
        debug!(msg = "create Kanidm client", namespace, name);

        let client = KanidmClientBuilder::new()
            .danger_accept_invalid_certs(true)
            // TODO: ensure that URL matches the service name and port programmatically
            .address(format!("https://{name}.{namespace}.svc:8443"))
            .connect_timeout(5)
            .build()
            .map_err(|e| {
                Error::KanidmClientError("failed to build Kanidm client".to_string(), Box::new(e))
            })?;

        let secret_api = Api::<Secret>::namespaced(k_client.clone(), namespace);
        let secret_name = format!("{name}-admin-passwords");
        trace!(
            msg = format!("fetch Kanidm {user} password"),
            namespace,
            name,
            secret_name
        );
        let admin_secret = secret_api.get(&secret_name).await.map_err(|e| {
            Error::KubeError(
                format!("failed to get secret: {namespace}/{secret_name}"),
                e,
            )
        })?;
        let secret_data = admin_secret.data.ok_or_else(|| {
            Error::MissingData(format!(
                "failed to get data in secret: {namespace}/{secret_name}"
            ))
        })?;

        let username = serde_plain::to_string(&user).unwrap();
        let password_bytes = secret_data.get(&username).ok_or_else(|| {
            Error::MissingData(format!(
                "missing password for {user} in secret: {namespace}/{secret_name}"
            ))
        })?;

        let password = std::str::from_utf8(&password_bytes.0)
            .map_err(|e| Error::Utf8Error("failed to convert password to string".to_string(), e))?;
        trace!(
            msg = format!("authenticating with new client and user {user}"),
            namespace,
            name
        );
        client
            .auth_simple_password(&username, password)
            .await
            .map_err(|e| {
                Error::KanidmClientError("client failed to authenticate".to_string(), Box::new(e))
            })?;
        Ok(Arc::new(client))
    }
}

#[derive(Clone, PartialEq, Hash, Eq)]
pub struct KanidmKey {
    pub namespace: String,
    pub name: String,
}
