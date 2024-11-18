use crate::error::{Error, Result};
use crate::metrics::{ControllerMetrics, Metrics};

use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kaniop_k8s_util::types::short_type_name;

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ListParams, ResourceExt};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::Store;
use kube::runtime::reflector::{Lookup, ReflectHandle};
use kube::Resource;
use prometheus_client::registry::Registry;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::error;

pub type ControllerId = &'static str;

/// State shared between the controller and the web server
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
    /// Shared Kanidm cache clients
    kanidm_clients: Arc<RwLock<KanidmClients>>,
}

// TODO: make this dynamic form an Enum, and macro generate the struct based on the enum variants
/// defines store structs. E.g:
/// ```ignore
/// define_stores!(
///     stateful_set_store => Store<StatefulSet>,
///     service_store => Store<Service>,
/// );
/// ```
///
/// The above macro invocation will generate the following code:
/// ```ignore
/// #[derive(Clone, Default)]
/// pub struct Stores {
///    pub stateful_set_store: Option<Store<StatefulSet>>,
///    pub service_store: Option<Store<Service>>,
/// }
///
/// impl Stores {
///    pub fn new(stateful_set_store: Option<Store<StatefulSet>>, service_store: Option<Store<Service>>) -> Self {
///       Stores {
///           stateful_set_store,
///           service_store,
///      }
///   }
///
///  pub fn stateful_set_store(&self) -> &Store<StatefulSet> {
///     self.stateful_set_store.as_ref().expect("stateful_set_store store is not initialized")
/// }
///
/// pub fn service_store(&self) -> &Store<Service> {
///    self.service_store.as_ref().expect("service_store store is not initialized")
/// }
/// }
/// ```
macro_rules! define_stores {
    ($($variant:ident => $store:ident<$type:ty>),*) => {
        #[derive(Clone, Default)]
        pub struct Stores {
            $(pub $variant: Option<$store<$type>>),*
        }

        impl Stores {
            pub fn new($($variant: Option<$store<$type>>),*) -> Self {
                Stores {
                    $($variant),*
                }
            }

            $(
                pub fn $variant(&self) -> &$store<$type> {
                    self.$variant.as_ref().expect(format!("{} store is not initialized", stringify!($variant)).as_str())
                }
            )*
        }
    }
}

define_stores!(
    stateful_set_store => Store<StatefulSet>,
    service_store => Store<Service>,
    ingress_store => Store<Ingress>,
    secret_store => Store<Secret>
);

/// Shared state for a resource stream
pub struct ResourceReflector<K>
where
    K: Resource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    pub store: Store<K>,
    pub writer: Writer<K>,
    pub subscriber: ReflectHandle<K>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    pub fn new(registry: Registry, controller_names: &[&'static str]) -> Self {
        Self {
            metrics: Arc::new(Metrics::new(registry, controller_names)),
            kanidm_clients: Arc::new(RwLock::new(KanidmClients::new())),
        }
    }

    /// Metrics getter
    pub fn metrics(&self) -> Result<String> {
        let mut buffer = String::new();
        let registry = &*self.metrics.registry;
        prometheus_client::encoding::text::encode(&mut buffer, registry)
            .map_err(|e| Error::FormattingError("failed to encode metrics".to_string(), e))?;
        Ok(buffer)
    }

    /// Create a Controller Context that can update State
    pub fn to_context(
        &self,
        client: Client,
        controller_id: ControllerId,
        store: Stores,
    ) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: self
                .metrics
                .controllers
                .get(controller_id)
                .expect("all CONTROLLER_IDs have to be registered")
                .clone(),
            stores: Arc::new(store),
            kanidm_clients: self.kanidm_clients.clone(),
        })
    }
}

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<ControllerMetrics>,
    /// Shared store
    pub stores: Arc<Stores>,
    /// Shared Kanidm cache clients
    kanidm_clients: Arc<RwLock<KanidmClients>>,
}

impl Context {
    /// Return a valid client for the Kanidm cluster. This operation require to do at least a
    /// request for validating the client, use it wisely.
    pub async fn get_kanidm_client(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Arc<KanidmClient>> {
        let key = format!("{namespace}/{name}");
        if let Some(client) = self.kanidm_clients.read().await.0.get(&key) {
            if client.whoami().await.is_ok() {
                return Ok(client.clone());
            }
        }

        let client = KanidmClients::create_client(namespace, name, self.client.clone()).await?;
        self.kanidm_clients
            .write()
            .await
            .0
            .insert(key, client.clone());
        Ok(client)
    }
}

struct KanidmClients(HashMap<String, Arc<KanidmClient>>);

impl KanidmClients {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub async fn create_client(
        namespace: &str,
        name: &str,
        k_client: Client,
    ) -> Result<Arc<KanidmClient>> {
        let client = KanidmClientBuilder::new()
            .danger_accept_invalid_certs(true)
            // TODO: ensure that URL matches the service name and port programmatically
            .address(format!("https://{name}.{namespace}.svc:8443"))
            .build()
            .map_err(|e| {
                Error::KanidmClientError("failed to build Kanidm client".to_string(), Box::new(e))
            })?;

        let secret_api = Api::<Secret>::namespaced(k_client.clone(), namespace);
        let secret_name = format!("{name}-admin-passwords");
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
        const IDM_ADMIN_USER: &str = "idm_admin";
        let password_bytes = secret_data.get(IDM_ADMIN_USER).ok_or_else(|| {
            Error::MissingData(format!(
                "missing password for {IDM_ADMIN_USER} in secret: {namespace}/{secret_name}"
            ))
        })?;

        let password = std::str::from_utf8(&password_bytes.0)
            .map_err(|e| Error::Utf8Error("failed to convert password to string".to_string(), e))?;
        client
            .auth_simple_password(IDM_ADMIN_USER, password)
            .await
            .map_err(|e| {
                Error::KanidmClientError("client failed to authenticate".to_string(), Box::new(e))
            })?;
        Ok(Arc::new(client))
    }
}

pub async fn check_api_queryable<K>(client: Client) -> Api<K>
where
    K: Resource + Clone + DeserializeOwned + Debug,
    <K as Resource>::DynamicType: Default,
{
    let api = Api::<K>::all(client.clone());
    if let Err(e) = api.list(&ListParams::default().limit(1)).await {
        error!(
            "{} is not queryable; {e:?}. Check controller permissions",
            short_type_name::<K>().unwrap_or("Unknown resource"),
        );
        std::process::exit(1);
    }
    api
}

pub fn error_policy<K: ResourceExt>(obj: Arc<K>, error: &Error, ctx: Arc<Context>) -> Action {
    // safe unwrap: all resources in the operator are namespace scoped resources
    error!(msg = "failed reconciliation", namespace = %obj.namespace().unwrap(), name = %obj.name_any(), %error);
    ctx.metrics.reconcile_failure_inc();
    Action::requeue(Duration::from_secs(5 * 60))
}
