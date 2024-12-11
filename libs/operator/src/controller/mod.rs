pub mod context;
pub mod kanidm;

use self::{context::Context, kanidm::KanidmClients};

use crate::error::{Error, Result};
use crate::kanidm::crd::Kanidm;
use crate::metrics::Metrics;

use kaniop_k8s_util::events::Recorder;
use kaniop_k8s_util::types::short_type_name;

use std::fmt::Debug;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, ListParams};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{self, Lookup, ReflectHandle, Store};
use kube::Resource;
use prometheus_client::registry::Registry;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::error;

pub type ControllerId = &'static str;
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// State shared between the controller and the web server
// Kanidm defined as a generic because it causes a cycle dependency with the kaniop_kanidm crate
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
    /// Shared Kanidm cache clients with the ability to manage users and their groups
    idm_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache clients with the ability to manage the operation of Kanidm as a
    /// database and service
    system_clients: Arc<RwLock<KanidmClients>>,
    /// Cache for Namespace resources
    pub namespace_store: Store<Namespace>,
    /// Cache for Kanidm resources
    pub kanidm_store: Store<Kanidm>,
}

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
    pub fn new(
        registry: Registry,
        controller_names: &[&'static str],
        namespace_store: Store<Namespace>,
        kanidm_store: Store<Kanidm>,
    ) -> Self {
        Self {
            metrics: Arc::new(Metrics::new(registry, controller_names)),
            idm_clients: Arc::default(),
            system_clients: Arc::default(),
            namespace_store,
            kanidm_store,
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
    pub fn to_context<K>(&self, client: Client, controller_id: ControllerId) -> Context<K>
    where
        K: Resource + Lookup + Clone + 'static,
        <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
    {
        Context::new(
            controller_id,
            client.clone(),
            self.metrics
                .controllers
                .get(controller_id)
                .expect("all CONTROLLER_IDs have to be registered")
                .clone(),
            Recorder::new(client.clone(), controller_id.into()),
            self.idm_clients.clone(),
            self.system_clients.clone(),
            self.namespace_store.clone(),
            self.kanidm_store.clone(),
        )
    }
}

pub trait KanidmResource {
    fn kanidm_name(&self) -> String;
    fn kanidm_namespace(&self) -> String;
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

pub fn create_subscriber<K>(buffer_size: usize) -> ResourceReflector<K>
where
    K: Resource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    let (store, writer) = reflector::store_shared(buffer_size);
    let subscriber = writer
        .subscribe()
        .expect("subscribers can only be created from shared stores");

    ResourceReflector {
        store,
        writer,
        subscriber,
    }
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
            use $crate::controller::context::BackoffContext;
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
                    ctx.metrics().reconcile_failure_inc();
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