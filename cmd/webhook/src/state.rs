use crate::kanidm_client::KanidmClientCache;

use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use std::collections::HashMap;
use std::sync::Arc;

use kube::Client;
use kube::runtime::reflector::Store;
use tokio::sync::RwLock;

/// Type alias for entity references: (kanidm_name, kanidm_namespace)
type EntityRefs = Vec<(String, String)>;

/// Type alias for entity cache map
type EntityCacheMap = Arc<RwLock<HashMap<String, EntityRefs>>>;

/// Cache for internal entities synced from Kanidm clusters with external replication
#[derive(Clone, Default)]
pub struct InternalEntityCache {
    /// Maps entity name to list of Kanidm clusters that contain it
    /// Key: entity name, Value: list of (kanidm_name, kanidm_namespace)
    groups: EntityCacheMap,
    persons: EntityCacheMap,
    oauth2_clients: EntityCacheMap,
    service_accounts: EntityCacheMap,
}

impl InternalEntityCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a group to the cache
    pub async fn add_group(&self, name: String, kanidm_name: String, kanidm_namespace: String) {
        let mut groups = self.groups.write().await;
        groups
            .entry(name)
            .or_default()
            .push((kanidm_name, kanidm_namespace));
    }

    /// Add a person to the cache
    pub async fn add_person(&self, name: String, kanidm_name: String, kanidm_namespace: String) {
        let mut persons = self.persons.write().await;
        persons
            .entry(name)
            .or_default()
            .push((kanidm_name, kanidm_namespace));
    }

    /// Add an OAuth2 client to the cache
    pub async fn add_oauth2_client(
        &self,
        name: String,
        kanidm_name: String,
        kanidm_namespace: String,
    ) {
        let mut oauth2_clients = self.oauth2_clients.write().await;
        oauth2_clients
            .entry(name)
            .or_default()
            .push((kanidm_name, kanidm_namespace));
    }

    /// Add a service account to the cache
    pub async fn add_service_account(
        &self,
        name: String,
        kanidm_name: String,
        kanidm_namespace: String,
    ) {
        let mut service_accounts = self.service_accounts.write().await;
        service_accounts
            .entry(name)
            .or_default()
            .push((kanidm_name, kanidm_namespace));
    }

    /// Check if a group exists in internal cache for a given Kanidm cluster
    pub async fn has_group(&self, name: &str, kanidm_name: &str, kanidm_namespace: &str) -> bool {
        let groups = self.groups.read().await;
        groups
            .get(name)
            .map(|refs| {
                refs.iter()
                    .any(|(kn, kns)| kn == kanidm_name && kns == kanidm_namespace)
            })
            .unwrap_or(false)
    }

    /// Check if a person exists in internal cache for a given Kanidm cluster
    pub async fn has_person(&self, name: &str, kanidm_name: &str, kanidm_namespace: &str) -> bool {
        let persons = self.persons.read().await;
        persons
            .get(name)
            .map(|refs| {
                refs.iter()
                    .any(|(kn, kns)| kn == kanidm_name && kns == kanidm_namespace)
            })
            .unwrap_or(false)
    }

    /// Check if an OAuth2 client exists in internal cache for a given Kanidm cluster
    pub async fn has_oauth2_client(
        &self,
        name: &str,
        kanidm_name: &str,
        kanidm_namespace: &str,
    ) -> bool {
        let oauth2_clients = self.oauth2_clients.read().await;
        oauth2_clients
            .get(name)
            .map(|refs| {
                refs.iter()
                    .any(|(kn, kns)| kn == kanidm_name && kns == kanidm_namespace)
            })
            .unwrap_or(false)
    }

    /// Check if a service account exists in internal cache for a given Kanidm cluster
    pub async fn has_service_account(
        &self,
        name: &str,
        kanidm_name: &str,
        kanidm_namespace: &str,
    ) -> bool {
        let service_accounts = self.service_accounts.read().await;
        service_accounts
            .get(name)
            .map(|refs| {
                refs.iter()
                    .any(|(kn, kns)| kn == kanidm_name && kns == kanidm_namespace)
            })
            .unwrap_or(false)
    }

    /// Clear all cached entities for a specific Kanidm cluster
    pub async fn clear_kanidm(&self, kanidm_name: &str, kanidm_namespace: &str) {
        let mut groups = self.groups.write().await;
        for (_, refs) in groups.iter_mut() {
            refs.retain(|(kn, kns)| kn != kanidm_name || kns != kanidm_namespace);
        }
        groups.retain(|_, refs| !refs.is_empty());

        let mut persons = self.persons.write().await;
        for (_, refs) in persons.iter_mut() {
            refs.retain(|(kn, kns)| kn != kanidm_name || kns != kanidm_namespace);
        }
        persons.retain(|_, refs| !refs.is_empty());

        let mut oauth2_clients = self.oauth2_clients.write().await;
        for (_, refs) in oauth2_clients.iter_mut() {
            refs.retain(|(kn, kns)| kn != kanidm_name || kns != kanidm_namespace);
        }
        oauth2_clients.retain(|_, refs| !refs.is_empty());

        let mut service_accounts = self.service_accounts.write().await;
        for (_, refs) in service_accounts.iter_mut() {
            refs.retain(|(kn, kns)| kn != kanidm_name || kns != kanidm_namespace);
        }
        service_accounts.retain(|_, refs| !refs.is_empty());
    }
}

#[derive(Clone)]
pub struct WebhookState {
    pub group_store: Store<KanidmGroup>,
    pub person_store: Store<KanidmPersonAccount>,
    pub oauth2_store: Store<KanidmOAuth2Client>,
    pub service_account_store: Store<KanidmServiceAccount>,
    pub kanidm_store: Store<Kanidm>,
    pub internal_cache: InternalEntityCache,
    pub kanidm_client_cache: KanidmClientCache,
    pub kube_client: Client,
}

impl WebhookState {
    pub fn new(
        group_store: Store<KanidmGroup>,
        person_store: Store<KanidmPersonAccount>,
        oauth2_store: Store<KanidmOAuth2Client>,
        service_account_store: Store<KanidmServiceAccount>,
        kanidm_store: Store<Kanidm>,
        kube_client: Client,
    ) -> Self {
        Self {
            group_store,
            person_store,
            oauth2_store,
            service_account_store,
            kanidm_store,
            internal_cache: InternalEntityCache::new(),
            kanidm_client_cache: KanidmClientCache::new(),
            kube_client,
        }
    }
}
