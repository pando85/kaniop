use crate::error::{Error, Result};
use crate::metrics::{ControllerMetrics, Metrics};

use std::sync::Arc;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::api::networking::v1::Ingress;
use kube::client::Client;
use kube::runtime::reflector::Store;
use prometheus_client::registry::Registry;

pub type ControllerId = &'static str;

/// State shared between the controller and the web server
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
}

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
    ingress_store => Store<Ingress>
);

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
            .map_err(Error::FormattingError)?;
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
