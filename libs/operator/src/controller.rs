use crate::error::{Error, Result};
use crate::metrics::{ControllerMetrics, Metrics};

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use kube::client::Client;
use kube::runtime::reflector::{Lookup, Store};
use prometheus_client::registry::Registry;

pub type ControllerId = &'static str;

/// State shared between the controller and the web server
#[derive(Clone)]
pub struct State {
    /// Metrics
    metrics: Arc<Metrics>,
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
            .map_err(Error::FormattingError)?;
        Ok(buffer)
    }

    /// Create a Controller Context that can update State
    pub fn to_context<K: 'static + Lookup>(
        &self,
        client: Client,
        controller_id: ControllerId,
        store: HashMap<String, Box<Store<K>>>,
    ) -> Arc<Context<K>>
    where
        K::DynamicType: Hash + Eq,
    {
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
pub struct Context<K: 'static + Lookup>
where
    K::DynamicType: Hash + Eq,
{
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<ControllerMetrics>,
    /// Shared store
    pub stores: Arc<HashMap<String, Box<Store<K>>>>,
}
