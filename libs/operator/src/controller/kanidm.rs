use crate::{
    crd::KanidmRef,
    error::{Error, Result},
    kanidm::{
        crd::Kanidm,
        reconcile::secret::{
            ADMIN_PASSWORD_KEY, ADMIN_USER, IDM_ADMIN_PASSWORD_KEY, IDM_ADMIN_USER,
        },
    },
};

use kanidm_client::{KanidmClient, KanidmClientBuilder};

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use k8s_openapi::api::core::v1::{Namespace, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::ResourceExt;
use kube::api::Api;
use kube::client::Client;
use kube::core::{Selector, SelectorExt};
use kube::runtime::reflector::Store;
use serde::Serialize;
use tracing::{debug, trace};

pub trait KanidmResource: ResourceExt {
    /// Returns the KanidmRef from the resource's spec
    fn kanidm_ref_spec(&self) -> &KanidmRef;

    /// Returns the namespace selector field for this resource type from the Kanidm spec
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector>;

    /// Returns the name of the referenced Kanidm resource
    fn kanidm_name(&self) -> String {
        self.kanidm_ref_spec().name.clone()
    }

    /// Returns the namespace of the referenced Kanidm resource
    /// Uses the explicitly specified namespace in kanidm_ref, or falls back to the resource's own namespace
    fn kanidm_namespace(&self) -> String {
        self.kanidm_ref_spec()
            .namespace
            .clone()
            // safe unwrap: all resources implementing this trait are namespaced scoped
            .unwrap_or_else(|| self.namespace().unwrap())
    }

    /// Returns a string representation of the Kanidm reference in "namespace/name" format
    fn kanidm_ref(&self) -> String {
        format!("{}/{}", self.kanidm_namespace(), self.kanidm_name())
    }
}

/// Generic function to check if a resource is watched based on namespace selectors
///
/// This function implements the common logic for checking whether a resource should be
/// reconciled based on the namespace selector configuration in the referenced Kanidm resource.
pub fn is_resource_watched<T>(
    resource: &T,
    kanidm: &Kanidm,
    namespace_store: &Store<Namespace>,
) -> bool
where
    T: KanidmResource,
{
    // safe unwrap: all resources are namespaced
    let namespace = resource.namespace().unwrap();
    trace!(msg = "check if resource is watched");

    let namespace_selector = if let Some(selector) = T::get_namespace_selector(kanidm) {
        selector
    } else {
        trace!(msg = "no namespace selector found, defaulting to current namespace");
        // A null label selector (default value) matches the current namespace only
        return kanidm.namespace().unwrap() == namespace;
    };

    let selector: Selector = if let Ok(s) = namespace_selector.clone().try_into() {
        s
    } else {
        trace!(msg = "failed to parse namespace selector, defaulting to current namespace");
        return kanidm.namespace().unwrap() == namespace;
    };

    trace!(msg = "namespace selector", ?selector);
    namespace_store
        .state()
        .iter()
        .filter(|n| selector.matches(n.metadata.labels.as_ref().unwrap_or(&Default::default())))
        .any(|n| n.name_any() == namespace)
}

#[derive(Serialize, Clone, Debug)]
pub enum KanidmUser {
    IdmAdmin,
    Admin,
}

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
            // using Kanidm object from cache is the unique way
            .address(format!("https://{name}.{namespace}.svc:8443"))
            .connect_timeout(5)
            .build()
            .map_err(|e| {
                Error::KanidmClientError("failed to build Kanidm client".to_string(), Box::new(e))
            })?;

        let secret_api = Api::<Secret>::namespaced(k_client.clone(), namespace);
        let secret_name = format!("{name}-admin-passwords");
        let admin_secret = secret_api.get(&secret_name).await.map_err(|e| {
            Error::KubeError(
                format!("failed to get secret: {namespace}/{secret_name}"),
                Box::new(e),
            )
        })?;
        let secret_data = admin_secret.data.ok_or_else(|| {
            Error::MissingData(format!(
                "failed to get data in secret: {namespace}/{secret_name}"
            ))
        })?;

        let (username, password_key) = match user {
            KanidmUser::Admin => (ADMIN_USER, ADMIN_PASSWORD_KEY),
            KanidmUser::IdmAdmin => (IDM_ADMIN_USER, IDM_ADMIN_PASSWORD_KEY),
        };
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
            .map_err(|e| Error::Utf8Error("failed to convert password to string".to_string(), e))?;
        trace!(
            msg = format!("authenticating with new client and user {username}"),
            namespace, name
        );
        client
            .auth_simple_password(username, password)
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
