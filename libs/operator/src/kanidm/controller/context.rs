use crate::controller::context::BackoffContext;
use crate::metrics::ControllerMetrics;
use crate::{controller::context::Context as KaniopContext, kanidm::crd::Kanidm};

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::{Secret, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::runtime::reflector::{ObjectRef, Store};

#[derive(Clone)]
pub struct Context {
    pub kaniop_ctx: KaniopContext<Kanidm>,
    /// Shared store
    pub stores: Arc<Stores>,
}

impl Context {
    pub fn new(kaniop_ctx: KaniopContext<Kanidm>, stores: Stores) -> Self {
        Context {
            kaniop_ctx,
            stores: Arc::new(stores),
        }
    }
}

impl BackoffContext<Kanidm> for Context {
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        self.kaniop_ctx.metrics()
    }
    async fn get_backoff(&self, obj_ref: ObjectRef<Kanidm>) -> Duration {
        self.kaniop_ctx.get_backoff(obj_ref).await
    }

    async fn reset_backoff(&self, obj_ref: ObjectRef<Kanidm>) {
        self.kaniop_ctx.reset_backoff(obj_ref).await
    }
}

pub struct Stores {
    pub stateful_set_store: Store<StatefulSet>,
    pub service_store: Store<Service>,
    pub ingress_store: Store<Ingress>,
    pub secret_store: Store<Secret>,
}
