use kaniop_operator::kanidm::crd::Kanidm;

use std::collections::HashMap;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Secret;
use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kube::{Api, Client, ResourceExt};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, trace};

const IDM_ADMIN_USER: &str = "idm_admin";
const IDM_ADMIN_PASSWORD_KEY: &str = "idm_admin";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Kanidm client error: {0}: {1:?}")]
    KanidmClient(String, kanidm_client::ClientError),
    #[error("Kubernetes error: {0}")]
    Kube(String, #[source] kube::Error),
    #[error("Missing data: {0}")]
    MissingData(String),
    #[error("UTF-8 conversion error: {0}")]
    Utf8(String, #[source] std::str::Utf8Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, PartialEq, Hash, Eq, Debug)]
pub enum KanidmUser {
    IdmAdmin,
    Admin,
}

#[derive(Clone, PartialEq, Hash, Eq, Debug)]
pub struct KanidmKey {
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Hash, Eq, Debug)]
pub struct ClientLockKey {
    pub namespace: String,
    pub name: String,
    pub user: KanidmUser,
}

/// Cache of Kanidm clients
#[derive(Clone, Default)]
pub struct KanidmClientCache {
    /// Maps Kanidm cluster to authenticated client
    idm_clients: Arc<RwLock<HashMap<KanidmKey, Arc<KanidmClient>>>>,
    /// Locks to prevent concurrent client creation
    client_creation_locks: Arc<RwLock<HashMap<ClientLockKey, Arc<Mutex<()>>>>>,
}

impl KanidmClientCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a valid Kanidm client for the specified cluster
    /// Creates a new client if one doesn't exist or is invalid
    pub async fn get_client(
        &self,
        kanidm: &Kanidm,
        kube_client: Client,
    ) -> Result<Arc<KanidmClient>> {
        let namespace = kanidm
            .namespace()
            .ok_or_else(|| Error::MissingData("Kanidm namespace missing".to_string()))?;
        let name = kanidm.name_any();

        let key = KanidmKey {
            namespace: namespace.clone(),
            name: name.clone(),
        };

        // Fast path: check if we have a valid cached client
        if let Some(client) = self.get_valid_cached_client(&key, &namespace, &name).await {
            return Ok(client);
        }

        // Slow path: acquire lock to prevent concurrent creation
        let lock_key = ClientLockKey {
            namespace: namespace.clone(),
            name: name.clone(),
            user: KanidmUser::IdmAdmin,
        };

        let creation_lock = self
            .client_creation_locks
            .write()
            .await
            .entry(lock_key)
            .or_insert_with(Arc::default)
            .clone();

        let _guard = creation_lock.lock().await;

        // Double-check after acquiring lock
        if let Some(client) = self.get_valid_cached_client(&key, &namespace, &name).await {
            return Ok(client);
        }

        // Create new client
        let client = Self::create_client(&namespace, &name, kube_client).await?;
        self.idm_clients.write().await.insert(key, client.clone());

        Ok(client)
    }

    /// Check if a valid client exists in cache
    async fn get_valid_cached_client(
        &self,
        key: &KanidmKey,
        namespace: &str,
        name: &str,
    ) -> Option<Arc<KanidmClient>> {
        let client = self.idm_clients.read().await.get(key).cloned()?;

        trace!(
            msg = "check existing Kanidm client session",
            namespace, name
        );

        if client.auth_valid().await.is_ok() {
            trace!(msg = "reuse Kanidm client session", namespace, name);
            Some(client)
        } else {
            None
        }
    }

    /// Create a new Kanidm client and authenticate
    async fn create_client(
        namespace: &str,
        name: &str,
        k_client: Client,
    ) -> Result<Arc<KanidmClient>> {
        debug!(msg = "create Kanidm client", namespace, name);

        let client = KanidmClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .address(format!("https://{name}.{namespace}.svc:8443"))
            .connect_timeout(5)
            .build()
            .map_err(|e| Error::KanidmClient("failed to build Kanidm client".to_string(), e))?;

        let secret_api = Api::<Secret>::namespaced(k_client, namespace);
        let secret_name = format!("{name}-admin-passwords");
        let admin_secret = secret_api.get(&secret_name).await.map_err(|e| {
            Error::Kube(
                format!("failed to get secret: {namespace}/{secret_name}"),
                e,
            )
        })?;

        let secret_data = admin_secret.data.ok_or_else(|| {
            Error::MissingData(format!(
                "failed to get data in secret: {namespace}/{secret_name}"
            ))
        })?;

        let username = IDM_ADMIN_USER;
        let password_key = IDM_ADMIN_PASSWORD_KEY;

        trace!(
            msg = format!("fetch Kanidm {username} password"),
            namespace, name, secret_name
        );

        let password_bytes = secret_data.get(password_key).ok_or_else(|| {
            Error::MissingData(format!(
                "missing password for {username} in secret: {namespace}/{secret_name}"
            ))
        })?;

        let password = std::str::from_utf8(&password_bytes.0)
            .map_err(|e| Error::Utf8("failed to convert password to string".to_string(), e))?;

        trace!(
            msg = format!("authenticating with new client and user {username}"),
            namespace, name
        );

        client
            .auth_simple_password(username, password)
            .await
            .map_err(|e| Error::KanidmClient("client failed to authenticate".to_string(), e))?;

        Ok(Arc::new(client))
    }

    /// Remove a client from the cache (e.g., when Kanidm is deleted)
    pub async fn remove_client(&self, namespace: &str, name: &str) {
        let key = KanidmKey {
            namespace: namespace.to_string(),
            name: name.to_string(),
        };

        self.idm_clients.write().await.remove(&key);

        // Clean up locks
        let mut locks = self.client_creation_locks.write().await;
        locks.remove(&ClientLockKey {
            namespace: namespace.to_string(),
            name: name.to_string(),
            user: KanidmUser::IdmAdmin,
        });
        locks.remove(&ClientLockKey {
            namespace: namespace.to_string(),
            name: name.to_string(),
            user: KanidmUser::Admin,
        });
    }
}
