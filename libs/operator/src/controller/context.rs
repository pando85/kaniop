use super::{
    ControllerId, DEFAULT_RECONCILE_INTERVAL, KanidmClients,
    kanidm::{KanidmKey, KanidmResource, KanidmUser},
};

use crate::kanidm::crd::Kanidm;
use crate::metrics::ControllerMetrics;
use kaniop_k8s_util::error::{Error, Result};

use kaniop_k8s_util::types::short_type_name;

use std::collections::HashMap;
use std::sync::Arc;

use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};
use k8s_openapi::{NamespaceResourceScope, api::core::v1::Namespace};
use kanidm_client::KanidmClient;
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Api, client::Client};
use kube::{Resource, ResourceExt};
use kube::{
    api::{Patch, PatchParams},
    runtime::reflector::{Lookup, ObjectRef, Store},
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::{error, info, trace};

// Context for our reconciler
#[derive(Clone)]
pub struct Context<K: Resource> {
    /// Controller ID
    pub controller_id: ControllerId,
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<ControllerMetrics>,
    /// State of the error backoff policy per object
    error_backoff_cache: Arc<RwLock<HashMap<ObjectRef<K>, RwLock<ExponentialBackoff>>>>,
    /// Event recorder
    pub recorder: Recorder,
    /// Cache for Namespace resources
    pub namespace_store: Store<Namespace>,
    /// Cache for Kanidm resources
    pub kanidm_store: Store<Kanidm>,
    /// Shared Kanidm cache clients with the ability to manage users and their groups
    idm_clients: Arc<RwLock<KanidmClients>>,
    /// Shared Kanidm cache clients with the ability to manage the operation of Kanidm as a
    /// database and service
    system_clients: Arc<RwLock<KanidmClients>>,
}

impl<K> Context<K>
where
    K: Resource + ResourceExt + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        controller_id: ControllerId,
        client: Client,
        metrics: Arc<ControllerMetrics>,
        recorder: Recorder,
        idm_clients: Arc<RwLock<KanidmClients>>,
        system_clients: Arc<RwLock<KanidmClients>>,
        namespace_store: Store<Namespace>,
        kanidm_store: Store<Kanidm>,
    ) -> Self {
        Self {
            controller_id,
            client,
            metrics,
            recorder,
            namespace_store,
            kanidm_store,
            idm_clients,
            system_clients,
            error_backoff_cache: Arc::default(),
        }
    }

    pub async fn release_kanidm_clients(&self, kanidm: &Kanidm) {
        let key = KanidmKey {
            // safe unwrap: Kanidm is namespaced scoped
            namespace: kube::ResourceExt::namespace(kanidm).unwrap(),
            name: kanidm.name_any(),
        };

        {
            self.idm_clients.write().await.remove(&key);
        }
        {
            self.system_clients.write().await.remove(&key);
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
        let namespace = obj.kanidm_namespace();
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

        let client = { cache.read().await.get(&key).cloned() };

        if let Some(client) = client {
            trace!(
                msg = "check existing Kanidm client session",
                namespace, name
            );
            if client.auth_valid().await.is_ok() {
                trace!(msg = "reuse Kanidm client session", namespace, name);
                return Ok(client.clone());
            }
        }

        match KanidmClients::create_client(&namespace, &name, user, self.client.clone()).await {
            Ok(client) => {
                {
                    cache.write().await.insert(key.clone(), client.clone());
                }
                Ok(client)
            }
            Err(e) => {
                self.recorder
                    .publish(
                        &Event {
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
                        Error::KubeError("failed to publish event".to_string(), Box::new(e))
                    })?;
                Err(e)
            }
        }
    }

    /// Return [`Kanidm`] of the given object
    ///
    /// [`Kanidm`]: struct.Kanidm.html
    pub fn get_kanidm(&self, obj: &K) -> Option<Arc<Kanidm>> {
        let namespace = obj.kanidm_namespace();
        let name = obj.kanidm_name();
        self.kanidm_store.find(|k| {
            kube::ResourceExt::namespace(k).as_ref() == Some(&namespace) && k.name_any() == name
        })
    }
}

#[allow(async_fn_in_trait)]
pub trait BackoffContext<K: Resource> {
    fn metrics(&self) -> &Arc<ControllerMetrics>;
    async fn get_backoff(&self, obj_ref: ObjectRef<K>) -> Duration;
    async fn reset_backoff(&self, obj_ref: ObjectRef<K>);
}

impl<K> BackoffContext<K> for Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        &self.metrics
    }

    /// Return next duration of the backoff policy for the given object
    async fn get_backoff(&self, obj_ref: ObjectRef<K>) -> Duration {
        {
            let read_guard = self.error_backoff_cache.read().await;
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
        self.error_backoff_cache
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
    async fn reset_backoff(&self, obj_ref: ObjectRef<K>) {
        let read_guard = self.error_backoff_cache.read().await;
        if read_guard.get(&obj_ref).is_some() {
            drop(read_guard);
            trace!(
                msg = "reset backoff policy",
                namespace = obj_ref.namespace.as_deref().unwrap(),
                name = obj_ref.name
            );
            self.error_backoff_cache.write().await.remove(&obj_ref);
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait IdmClientContext<K: Resource> {
    async fn get_idm_client(&self, obj: &K) -> Result<Arc<KanidmClient>>;
}

impl<K> IdmClientContext<K> for Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + KanidmResource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    async fn get_idm_client(&self, obj: &K) -> Result<Arc<KanidmClient>> {
        self.get_kanidm_client(obj, KanidmUser::IdmAdmin).await
    }
}

#[allow(async_fn_in_trait)]
pub trait SystemClientContext<K: Resource> {
    async fn get_system_client(&self, obj: &K) -> Result<Arc<KanidmClient>>;
}

impl<K> SystemClientContext<K> for Context<K>
where
    K: Resource<DynamicType = ()> + ResourceExt + KanidmResource + Lookup + Clone + 'static,
    <K as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
{
    async fn get_system_client(&self, obj: &K) -> Result<Arc<KanidmClient>> {
        self.get_kanidm_client(obj, KanidmUser::Admin).await
    }
}

#[allow(async_fn_in_trait)]
pub trait KubeOperations<T, K>
where
    T: Resource + ResourceExt + Lookup + Clone + 'static,
    <T as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
    K: Resource<Scope = NamespaceResourceScope>
        + Serialize
        + Clone
        + std::fmt::Debug
        + for<'de> Deserialize<'de>,
    <K as kube::Resource>::DynamicType: Default,
    <K as Resource>::Scope: std::marker::Sized,
{
    async fn kube_delete(&self, client: Client, metrics: &ControllerMetrics, obj: &K)
    -> Result<()>;
    async fn kube_patch(
        &self,
        client: Client,
        metrics: &ControllerMetrics,
        obj: K,
        operator_name: &str,
    ) -> Result<K>;
}

impl<T, K> KubeOperations<T, K> for T
where
    T: Resource + ResourceExt + Lookup + Clone + 'static,
    <T as Lookup>::DynamicType: Eq + std::hash::Hash + Clone,
    K: Resource<Scope = NamespaceResourceScope>
        + Serialize
        + Clone
        + std::fmt::Debug
        + for<'de> Deserialize<'de>,
    <K as kube::Resource>::DynamicType: Default,
    <K as Resource>::Scope: std::marker::Sized,
{
    async fn kube_delete(
        &self,
        client: Client,
        _metrics: &ControllerMetrics,
        obj: &K,
    ) -> Result<()> {
        let name = obj.name_any();
        // safe unwrap: self is namespaced scoped
        let namespace = kube::ResourceExt::namespace(self).unwrap();
        trace!(
            msg = format!("deleting {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let api = Api::<K>::namespaced(client, &namespace);
        api.delete(&name, &Default::default()).await.map_err(|e| {
            Error::KubeError(
                format!(
                    "failed to delete {} {namespace}/{name}",
                    short_type_name::<K>().unwrap_or("Unknown")
                ),
                Box::new(e),
            )
        })?;
        Ok(())
    }

    async fn kube_patch(
        &self,
        client: Client,
        metrics: &ControllerMetrics,
        obj: K,
        operator_name: &str,
    ) -> Result<K> {
        let name = obj.name_any();
        // safe unwrap: self is namespaced scoped
        let namespace = kube::ResourceExt::namespace(self).unwrap();
        trace!(
            msg = format!("patching {}", short_type_name::<K>().unwrap_or("Unknown")),
            resource.name = &name,
            resource.namespace = &namespace
        );
        let resource_api = Api::<K>::namespaced(client.clone(), &namespace);

        let result = resource_api
            .patch(
                &name,
                &PatchParams::apply(operator_name).force(),
                &Patch::Apply(&obj),
            )
            .await;
        match result {
            Ok(resource) => Ok(resource),
            Err(e) => match e {
                kube::Error::Api(ae) if ae.code == 422 => {
                    info!(
                        msg = format!(
                            "recreating {} because the update operation was not possible",
                            short_type_name::<K>().unwrap_or("Unknown")
                        ),
                        reason = ae.reason
                    );
                    trace!(msg = "operation was not possible because of 422", ?ae);
                    self.kube_delete(client.clone(), metrics, &obj).await?;
                    metrics.reconcile_deploy_delete_create_inc();
                    resource_api
                        .patch(
                            &name,
                            &PatchParams::apply(operator_name).force(),
                            &Patch::Apply(&obj),
                        )
                        .await
                        .map_err(|e| {
                            Error::KubeError(
                                format!(
                                    "failed to re-try patch {} {namespace}/{name}",
                                    short_type_name::<K>().unwrap_or("Unknown")
                                ),
                                Box::new(e),
                            )
                        })
                }
                _ => Err(Error::KubeError(
                    format!(
                        "failed to patch {} {namespace}/{name}",
                        short_type_name::<K>().unwrap_or("Unknown")
                    ),
                    Box::new(e),
                )),
            },
        }
    }
}
