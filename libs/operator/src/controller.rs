use crate::error::{Error, Result};
use crate::metrics::{ControllerMetrics, Metrics};

use std::sync::Arc;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::client::Client;
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::Store;
use kube::runtime::reflector::{Lookup, ReflectHandle};
use kube::Resource;
use prometheus_client::registry::Registry;

pub type ControllerId = &'static str;

/// State shared between the controller and the web server
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
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
/// #[derive(Clone)]
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
/// }
/// ```
macro_rules! define_stores {
    ($($variant:ident => $store:ident<$type:ty>),*) => {
        #[derive(Clone)]
        pub struct Stores {
            $(pub $variant: Option<$store<$type>>),*
        }

        impl Stores {
            pub fn new($($variant: Option<$store<$type>>),*) -> Self {
                Stores {
                    $($variant),*
                }
            }
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
}
