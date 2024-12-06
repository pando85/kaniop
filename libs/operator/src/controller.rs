use crate::error::{Error, Result};
use crate::metrics::{ControllerMetrics, Metrics};

use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kaniop_k8s_util::events::{Event, EventType, Recorder};
use kaniop_k8s_util::types::short_type_name;
use serde_plain::derive_display_from_serialize;

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, ListParams};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{Lookup, ObjectRef, ReflectHandle, Store};
use kube::{Resource, ResourceExt};
use prometheus_client::registry::Registry;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::{debug, error, trace};

pub type ControllerId = &'static str;
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// State shared between the controller and the web server
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
    /// Shared Kanidm cache clients with the ability to manage users and their groups
    idm_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache clients with the ability to manage the operation of Kanidm as a
    /// database and service
    system_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache domains
    kanidm_domains: Arc<RwLock<HashMap<KanidmKey, String>>>,
}

#[derive(Clone, PartialEq, Hash, Eq)]
pub struct KanidmKey {
    pub namespace: String,
    pub name: String,
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
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
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
            idm_clients: Arc::default(),
            system_clients: Arc::default(),
            kanidm_domains: Arc::default(),
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
    pub fn to_context<K>(
        &self,
        client: Client,
        controller_id: ControllerId,
        store: Stores,
    ) -> Arc<Context<K>>
    where
        K: Resource + Lookup + Clone + 'static,
        <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
    {
        Arc::new(Context {
            controller_id,
            recorder: Recorder::new(client.clone(), controller_id.into()),
            client,
            metrics: self
                .metrics
                .controllers
                .get(controller_id)
                .expect("all CONTROLLER_IDs have to be registered")
                .clone(),
            stores: Arc::new(store),
            idm_clients: self.idm_clients.clone(),
            system_clients: self.system_clients.clone(),
            kanidm_domains: self.kanidm_domains.clone(),
            internal_cache: Arc::default(),
            error_backoff_policy: Arc::default(),
        })
    }
}

// Context for our reconciler
#[derive(Clone)]
pub struct Context<K: Resource> {
    /// Controller ID
    pub controller_id: ControllerId,
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<ControllerMetrics>,
    /// Shared store
    pub stores: Arc<Stores>,
    /// Shared Kanidm cache clients with the ability to manage users and their groups
    idm_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache clients with the ability to manage the operation of Kanidm as a
    /// database and service
    system_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache domains
    kanidm_domains: Arc<RwLock<HashMap<KanidmKey, String>>>,
    // TODO: use this just in person account controller. Is UID better than ObjectRef?
    /// Internal controller cache
    pub internal_cache: Arc<RwLock<HashMap<ObjectRef<K>, time::OffsetDateTime>>>,
    /// State of the error backoff policy per object
    error_backoff_policy: Arc<RwLock<HashMap<ObjectRef<K>, RwLock<ExponentialBackoff>>>>,
    /// Event recorder
    pub recorder: Recorder,
}

impl<K> Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    /// Return next duration of the backoff policy for the given object
    pub async fn get_backoff(&self, obj_ref: ObjectRef<K>) -> Duration {
        {
            let read_guard = self.error_backoff_policy.read().await;
            if let Some(backoff) = read_guard.get(&obj_ref) {
                if let Some(duration) = backoff.write().await.next() {
                    return duration;
                }
            }
        }

        // Backoff policy: 1s, 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s, 300s, 300s...
        let mut backoff = ExponentialBuilder::default()
            .with_max_delay(DEFAULT_RECONCILE_INTERVAL)
            .without_max_times()
            .build();
        // safe unwrap: first backoff is always Some(Duration)
        let duration = backoff.next().unwrap();
        self.error_backoff_policy
            .write()
            .await
            .insert(obj_ref.clone(), RwLock::new(backoff));
        trace!(
            msg = format!("recreate backoff policy"),
            namespace = obj_ref.namespace.as_deref().unwrap(),
            name = obj_ref.name,
        );
        duration
    }

    /// Reset the backoff policy for the given object
    pub async fn reset_backoff(&self, obj_ref: ObjectRef<K>) {
        let read_guard = self.error_backoff_policy.read().await;
        if read_guard.get(&obj_ref).is_some() {
            drop(read_guard);
            trace!(
                msg = "reset backoff policy",
                namespace = obj_ref.namespace.as_deref().unwrap(),
                name = obj_ref.name
            );
            let mut write_guard = self.error_backoff_policy.write().await;
            write_guard.remove(&obj_ref);
        }
    }
}

impl<K> Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + KanidmResource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    /// Return a valid client for the Kanidm cluster. This operation require to do at least a
    /// request for validating the client, use it wisely.
    async fn get_kanidm_client(&self, obj: &K, user: KanidmUser) -> Result<Arc<KanidmClient>> {
        // safe unwrap: all resources in the operator are namespace scoped resources
        let namespace = kube::ResourceExt::namespace(obj).unwrap();
        let name = obj.kanidm_name();

        let cache = match user {
            KanidmUser::Admin => self.system_clients.clone(),
            KanidmUser::IdmAdmin => self.idm_clients.clone(),
        };

        trace!(msg = "try to reuse Kanidm client", namespace, name);

        let key = KanidmKey {
            namespace: namespace.clone(),
            name: name.clone(),
        };

        if let Some(client) = cache.read().await.0.get(&key) {
            trace!(
                msg = "check existing Kanidm client session",
                namespace,
                name
            );
            if client.auth_valid().await.is_ok() {
                trace!(msg = "reuse Kanidm client session", namespace, name);
                return Ok(client.clone());
            }
        }

        match KanidmClients::create_client(&namespace, &name, user, self.client.clone()).await {
            Ok(client) => {
                cache.write().await.0.insert(key.clone(), client.clone());

                if self.kanidm_domains.read().await.get(&key).is_none() {
                    let gvk = GroupVersionKind::gvk("kaniop.rs", "v1beta1", "Kanidm");
                    let kanidm_api = Api::<DynamicObject>::namespaced_with(
                        self.client.clone(),
                        &namespace,
                        &ApiResource::from_gvk(&gvk),
                    );
                    let kanidm = kanidm_api.get(&name).await.map_err(|e| {
                        Error::KubeError(format!("failed to get Kanidm: {namespace}/{name}"), e)
                    })?;
                    self.kanidm_domains.write().await.insert(
                        key.clone(),
                        kanidm
                            .data
                            .get("spec")
                            .unwrap()
                            .get("domain")
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string(),
                    );
                };
                Ok(client)
            }
            Err(e) => {
                self.recorder
                    .publish(
                        Event {
                            type_: EventType::Warning,
                            reason: "KanidmClientError".to_string(),
                            note: Some(e.to_string()),
                            action: "KanidmClientCreating".into(),
                            secondary: None,
                        },
                        &obj.object_ref(&()),
                    )
                    .await
                    .map_err(|e| {
                        error!(msg = "failed to create Kanidm client", %e);
                        Error::KubeError("failed to publish event".to_string(), e)
                    })?;
                Err(e)
            }
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait ContextKanidmClient<K: Resource> {
    async fn get_idm_client(&self, obj: &K) -> Result<Arc<KanidmClient>>;
    async fn get_system_client(&self, obj: &K) -> Result<Arc<KanidmClient>>;
    async fn get_domain(&self, obj: &K) -> Option<String>;
}

impl<K> ContextKanidmClient<K> for Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + KanidmResource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    async fn get_idm_client(&self, obj: &K) -> Result<Arc<KanidmClient>> {
        self.get_kanidm_client(obj, KanidmUser::IdmAdmin).await
    }

    async fn get_system_client(&self, obj: &K) -> Result<Arc<KanidmClient>> {
        self.get_kanidm_client(obj, KanidmUser::Admin).await
    }

    /// Return the domain of the Kanidm cluster of the given object
    async fn get_domain(&self, obj: &K) -> Option<String> {
        // safe unwrap: all resources in the operator are namespace scoped resources
        let namespace = kube::ResourceExt::namespace(obj).unwrap();
        let name = obj.kanidm_name();
        self.kanidm_domains
            .read()
            .await
            .get(&KanidmKey {
                namespace: namespace.to_string(),
                name: name.to_string(),
            })
            .cloned()
    }
}

pub trait KanidmResource {
    fn kanidm_name(&self) -> String;
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
enum KanidmUser {
    IdmAdmin,
    Admin,
}

derive_display_from_serialize!(KanidmUser);

#[derive(Default)]
struct KanidmClients(HashMap<KanidmKey, Arc<KanidmClient>>);

impl KanidmClients {
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

pub fn error_policy<K>(_obj: Arc<K>, _error: &Error, _ctx: Arc<Context<K>>) -> Action
where
    K: Resource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    unreachable!("Handle in backoff_reconciler macro")
}

#[macro_export]
macro_rules! backoff_reconciler {
    ($inner_reconciler:ident) => {
        |obj, ctx| async move {
            match $inner_reconciler(obj.clone(), ctx.clone()).await {
                Ok(action) => {
                    ctx.reset_backoff(kube::runtime::reflector::ObjectRef::from(obj.as_ref()))
                        .await;
                    Ok(action)
                }
                Err(error) => {
                    // safe unwrap: all resources in the operator are namespace scoped resources
                    let namespace = kube::ResourceExt::namespace(obj.as_ref()).unwrap();
                    let name = kube::ResourceExt::name_any(obj.as_ref());
                    tracing::error!(msg = "failed reconciliation", %namespace, %name, %error);
                    ctx.metrics.reconcile_failure_inc();
                    let backoff_duration = ctx
                        .get_backoff(kube::runtime::reflector::ObjectRef::from(obj.as_ref()))
                        .await;
                    tracing::trace!(
                        msg = format!("backoff duration: {backoff_duration:?}"),
                        %namespace,
                        %name,
                    );
                    Ok(kube::runtime::controller::Action::requeue(backoff_duration))
                }
            }
        }
    };
}
