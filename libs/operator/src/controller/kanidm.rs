use crate::{
    crd::KanidmRef,
    kanidm::{
        crd::Kanidm,
        reconcile::secret::{
            ADMIN_PASSWORD_KEY, ADMIN_USER, IDM_ADMIN_PASSWORD_KEY, IDM_ADMIN_USER,
        },
    },
};

use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kaniop_k8s_util::error::{Error, Result};

use std::collections::HashMap;
use std::fmt::Debug;
use std::io::Write;
use std::sync::Arc;

use k8s_openapi::api::core::v1::{Namespace, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::ResourceExt;
use kube::api::Api;
use kube::client::Client;
use kube::core::{Selector, SelectorExt};
use kube::runtime::reflector::Store;
use openssl::x509::X509;
use serde::Serialize;
use tempfile::NamedTempFile;
use tracing::{debug, trace};

pub trait KanidmResource: ResourceExt {
    /// Returns the KanidmRef from the resource's spec
    fn kanidm_ref_spec(&self) -> &KanidmRef;

    /// Returns the namespace selector field for this resource type from the Kanidm spec
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector>;

    /// Returns the optional Kanidm entity name override from the spec
    fn kanidm_name_override(&self) -> Option<&str>;

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

    /// Returns the entity name to use in Kanidm.
    /// If `kanidmName` is specified in the spec, uses that; otherwise uses the K8s resource name.
    fn kanidm_entity_name(&self) -> String {
        self.kanidm_name_override()
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.name_any())
    }
}

/// Check if a LabelSelector matches all namespaces (empty selector with no constraints)
fn selector_matches_all(selector: &LabelSelector) -> bool {
    selector.match_labels.is_none() || selector.match_labels.as_ref().is_some_and(|l| l.is_empty())
}

/// Generic function to check if a resource is watched based on namespace selectors
///
/// This function implements the common logic for checking whether a resource should be
/// reconciled based on the namespace selector configuration in the referenced Kanidm resource.
pub async fn is_resource_watched<T>(
    resource: &T,
    kanidm: &Kanidm,
    namespace_store: &Store<Namespace>,
    k8s_client: &Client,
) -> bool
where
    T: KanidmResource,
{
    let namespace = resource.namespace().unwrap();
    trace!(%namespace, "check if resource is watched");

    let namespace_selector = if let Some(selector) = T::get_namespace_selector(kanidm) {
        selector
    } else {
        trace!("no namespace selector found, defaulting to current namespace");
        return kanidm.namespace().unwrap() == namespace;
    };

    if selector_matches_all(namespace_selector) {
        trace!("namespace selector matches all namespaces, fast-track accepted");
        return true;
    }

    let selector: Selector = if let Ok(s) = namespace_selector.clone().try_into() {
        s
    } else {
        trace!("failed to parse namespace selector, defaulting to current namespace");
        return kanidm.namespace().unwrap() == namespace;
    };

    trace!(?selector, "namespace selector");

    let found_in_store = namespace_store
        .state()
        .iter()
        .filter(|n| selector.matches(n.metadata.labels.as_ref().unwrap_or(&Default::default())))
        .any(|n| n.name_any() == namespace);

    if found_in_store {
        return true;
    }

    trace!(%namespace, "namespace not found in store, fetching from K8s API");
    let namespace_api: Api<Namespace> = Api::all(k8s_client.clone());
    match namespace_api.get(&namespace).await {
        Ok(ns) => {
            let matches =
                selector.matches(ns.metadata.labels.as_ref().unwrap_or(&Default::default()));
            trace!(%namespace, matches, "namespace fetched from API");
            matches
        }
        Err(e) => {
            trace!(%namespace, ?e, "failed to fetch namespace from API, treating as not watched");
            false
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum KanidmUser {
    IdmAdmin,
    Admin,
}

const TLS_CERT_KEY: &str = "tls.crt";

fn tls_trust_anchor(secret: &Secret, namespace: &str, secret_name: &str) -> Result<Vec<u8>> {
    let data = secret.data.as_ref().ok_or_else(|| {
        Error::MissingData(format!(
            "failed to get data in TLS secret: {namespace}/{secret_name}"
        ))
    })?;
    let certificate_bundle = data.get(TLS_CERT_KEY).ok_or_else(|| {
        Error::MissingData(format!(
            "missing {TLS_CERT_KEY} in TLS secret: {namespace}/{secret_name}"
        ))
    })?;
    let certificates = X509::stack_from_pem(&certificate_bundle.0).map_err(|e| {
        Error::ParseError(format!(
            "failed to parse {TLS_CERT_KEY} from TLS secret {namespace}/{secret_name}: {e}"
        ))
    })?;
    let trust_anchor = certificates.last().ok_or_else(|| {
        Error::MissingData(format!(
            "no certificates found in {TLS_CERT_KEY} from TLS secret {namespace}/{secret_name}"
        ))
    })?;
    trust_anchor.to_pem().map_err(|e| {
        Error::ParseError(format!(
            "failed to encode trust anchor from TLS secret {namespace}/{secret_name}: {e}"
        ))
    })
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

    pub fn remove(&mut self, key: &KanidmKey) -> Option<Arc<KanidmClient>> {
        let client = self.0.remove(key);
        self.0.shrink_to_fit();
        client
    }

    pub async fn create_client(
        namespace: &str,
        name: &str,
        user: KanidmUser,
        k_client: Client,
    ) -> Result<Arc<KanidmClient>> {
        debug!(namespace, name, "create Kanidm client");

        let secret_api = Api::<Secret>::namespaced(k_client.clone(), namespace);
        let kanidm_api = Api::<Kanidm>::namespaced(k_client.clone(), namespace);
        let kanidm = kanidm_api.get(name).await.map_err(|e| {
            Error::KubeError(
                format!("failed to get Kanidm: {namespace}/{name}"),
                Box::new(e),
            )
        })?;
        let tls_secret_name = kanidm.effective_tls_secret_name();
        let tls_secret = secret_api.get(&tls_secret_name).await.map_err(|e| {
            Error::KubeError(
                format!("failed to get TLS secret: {namespace}/{tls_secret_name}"),
                Box::new(e),
            )
        })?;
        let trust_anchor = tls_trust_anchor(&tls_secret, namespace, &tls_secret_name)?;
        let mut trust_anchor_file = NamedTempFile::new_in("/tmp").map_err(|e| {
            Error::ParseError(format!(
                "failed to create temporary Kanidm trust anchor: {e}"
            ))
        })?;
        trust_anchor_file.write_all(&trust_anchor).map_err(|e| {
            Error::ParseError(format!(
                "failed to write temporary Kanidm trust anchor: {e}"
            ))
        })?;
        let trust_anchor_path = trust_anchor_file.path().to_str().ok_or_else(|| {
            Error::ParseError("temporary Kanidm trust anchor path is not valid UTF-8".to_string())
        })?;

        let client = KanidmClientBuilder::new()
            // The operator connects to the Kubernetes Service DNS name while Kanidm's certificate
            // represents its configured public domain. Verify the certificate chain against the
            // exact TLS Secret, but do not pretend the Service DNS name is present in the SAN.
            .enable_native_ca_roots(false)
            .danger_accept_invalid_hostnames(true)
            .add_root_certificate_filepath(trust_anchor_path)
            .map_err(|e| {
                Error::KanidmClientError(
                    "failed to configure Kanidm TLS trust".to_string(),
                    Box::new(e),
                )
            })?
            .address(format!("https://{name}.{namespace}.svc:8443"))
            .connect_timeout(5)
            .build()
            .map_err(|e| {
                Error::KanidmClientError("failed to build Kanidm client".to_string(), Box::new(e))
            })?;
        drop(trust_anchor_file);

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
            namespace,
            name, secret_name, "fetch Kanidm {username} password"
        );
        let password_bytes = secret_data.get(password_key).ok_or_else(|| {
            Error::MissingData(format!(
                "missing password for {username} in secret: {namespace}/{secret_name}"
            ))
        })?;

        let password = std::str::from_utf8(&password_bytes.0)
            .map_err(|e| Error::Utf8Error("failed to convert password to string".to_string(), e))?;
        trace!(
            namespace,
            name, "authenticating with new client and user {username}"
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

#[derive(Clone, PartialEq, Hash, Eq, Debug)]
pub struct ClientLockKey {
    pub namespace: String,
    pub name: String,
    pub user: KanidmUser,
}
