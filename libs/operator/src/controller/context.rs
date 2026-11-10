use super::{
    ControllerId, KanidmClients, idm_reconcile_interval,
    kanidm::{ClientLockKey, KanidmKey, KanidmResource, KanidmUser},
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
use tokio::sync::{Mutex, RwLock};
use tokio::time::Duration;
use tracing::{debug, error, info, trace};

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
    /// Locks for client creation to prevent thundering herd problem
    client_creation_locks: Arc<RwLock<HashMap<ClientLockKey, Arc<Mutex<()>>>>>,
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
            client_creation_locks: Arc::default(),
        }
    }

    pub async fn release_kanidm_clients(&self, kanidm: &Kanidm) {
        let key = KanidmKey {
            // safe unwrap: Kanidm is namespaced scoped
            namespace: kube::ResourceExt::namespace(kanidm).unwrap(),
            name: kanidm.name_any(),
        };

        self.idm_clients.write().await.remove(&key);
        self.system_clients.write().await.remove(&key);
        {
            let mut locks = self.client_creation_locks.write().await;
            locks.remove(&ClientLockKey {
                namespace: key.namespace.clone(),
                name: key.name.clone(),
                user: KanidmUser::IdmAdmin,
            });
            locks.remove(&ClientLockKey {
                namespace: key.namespace.clone(),
                name: key.name.clone(),
                user: KanidmUser::Admin,
            });
        }
    }

    /// Return a valid client for the given Kanidm cluster and user, reusing the shared client
    /// cache. A new client, and therefore a new Kanidm session, is only created when there is no
    /// cached client or its session is no longer valid.
    pub async fn get_kanidm_cluster_client(
        &self,
        kanidm: &Kanidm,
        user: KanidmUser,
    ) -> Result<Arc<KanidmClient>> {
        // safe unwrap: Kanidm is namespaced scoped
        let namespace = kube::ResourceExt::namespace(kanidm).unwrap();
        self.get_cached_client(&namespace, &kanidm.name_any(), user)
            .await
    }

    /// Check if a valid client exists in cache
    async fn get_valid_cached_client(
        cache: &Arc<RwLock<KanidmClients>>,
        key: &KanidmKey,
        namespace: &str,
        name: &str,
    ) -> Option<Arc<KanidmClient>> {
        let client = cache.read().await.get(key).cloned()?;

        trace!(namespace, name, "check existing Kanidm client session");
        if client.auth_valid().await.is_ok() {
            trace!(namespace, name, "reuse Kanidm client session");
            Some(client)
        } else {
            None
        }
    }

    /// Return a valid client from the cache, creating and caching a new one when needed. This
    /// operation require to do at least a request for validating the client, use it wisely.
    async fn get_cached_client(
        &self,
        namespace: &str,
        name: &str,
        user: KanidmUser,
    ) -> Result<Arc<KanidmClient>> {
        debug!(namespace, name, "get Kanidm client");

        let cache = match user {
            KanidmUser::Admin => self.system_clients.clone(),
            KanidmUser::IdmAdmin => self.idm_clients.clone(),
        };
        let key = KanidmKey {
            namespace: namespace.to_string(),
            name: name.to_string(),
        };
        if let Some(client) = Self::get_valid_cached_client(&cache, &key, namespace, name).await {
            return Ok(client);
        }

        // Slow path: acquire lock for this specific client to prevent concurrent creation
        let creation_lock = self
            .client_creation_locks
            .write()
            .await
            .entry(ClientLockKey {
                namespace: namespace.to_string(),
                name: name.to_string(),
                user: user.clone(),
            })
            .or_insert_with(Arc::default)
            .clone();
        let _guard = creation_lock.lock().await;

        // Double-check: another task may have created the client while we waited for the lock
        if let Some(client) = Self::get_valid_cached_client(&cache, &key, namespace, name).await {
            return Ok(client);
        }

        let client =
            KanidmClients::create_client(namespace, name, user, self.client.clone()).await?;
        cache.write().await.insert(key, client.clone());
        Ok(client)
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

        match self.get_cached_client(&namespace, &name, user).await {
            Ok(client) => Ok(client),
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
                        error!(%e, "failed to create Kanidm client");
                        Error::KubeError("failed to publish event".to_string(), Box::new(e))
                    })?;
                Err(e)
            }
        }
    }

    /// Return true when normal identity reconciliation may mutate the target Kanidm.
    pub fn kanidm_write_allowed(&self, obj: &K) -> bool {
        self.get_kanidm(obj).is_none_or(|kanidm| {
            !kanidm
                .annotations()
                .contains_key(crate::kanidm::restore::RESTORE_ANNOTATION)
        })
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
            .with_max_delay(idm_reconcile_interval())
            .without_max_times()
            .build();
        // First backoff is always Some(Duration), but use defensive fallback
        let duration = backoff.next().unwrap_or_else(|| {
            trace!("backoff returned None, using default duration");
            Duration::from_secs(1)
        });
        let mut cache = self.error_backoff_cache.write().await;
        use std::collections::hash_map::Entry;
        match cache.entry(obj_ref.clone()) {
            Entry::Vacant(vacant) => {
                vacant.insert(RwLock::new(backoff));
                self.metrics.objects_in_backoff_inc();
            }
            Entry::Occupied(_occupied) => {}
        }
        trace!(
            namespace = obj_ref.namespace.as_deref().unwrap(),
            name = obj_ref.name,
            "recreate backoff policy"
        );
        duration
    }

    /// Reset the backoff policy for the given object
    async fn reset_backoff(&self, obj_ref: ObjectRef<K>) {
        let removed = self.error_backoff_cache.write().await.remove(&obj_ref);
        if removed.is_some() {
            trace!(
                namespace = obj_ref.namespace.as_deref().unwrap(),
                name = obj_ref.name,
                "reset backoff policy"
            );
            self.metrics.objects_in_backoff_dec();
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

fn is_immutable_update_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    message.contains("field is immutable")
        || message.contains("may not change once set")
        || message.contains("may not be changed once set")
        || (message.contains("updates to statefulset spec for fields other than")
            && message.contains("are forbidden"))
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
    async fn kube_apply(&self, client: Client, obj: K, operator_name: &str) -> Result<K>;
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
            resource.name = &name,
            resource.namespace = &namespace,
            "deleting {}",
            short_type_name::<K>().unwrap_or("Unknown")
        );
        let api = Api::<K>::namespaced(client, &namespace);
        match api.delete(&name, &Default::default()).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                trace!(
                    resource.name = &name,
                    resource.namespace = &namespace,
                    "{} not found, treating delete as successful",
                    short_type_name::<K>().unwrap_or("Unknown")
                );
                Ok(())
            }
            Err(e) => Err(Error::KubeError(
                format!(
                    "failed to delete {} {namespace}/{name}",
                    short_type_name::<K>().unwrap_or("Unknown")
                ),
                Box::new(e),
            )),
        }
    }

    async fn kube_apply(&self, client: Client, obj: K, operator_name: &str) -> Result<K> {
        let name = obj.name_any();
        // safe unwrap: self is namespaced scoped
        let namespace = kube::ResourceExt::namespace(self).unwrap();
        trace!(
            resource.name = &name,
            resource.namespace = &namespace,
            "applying {}",
            short_type_name::<K>().unwrap_or("Unknown")
        );
        let resource_api = Api::<K>::namespaced(client, &namespace);

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
                        "failed to patch {} {namespace}/{name}",
                        short_type_name::<K>().unwrap_or("Unknown")
                    ),
                    Box::new(e),
                )
            })
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
        let result = self
            .kube_apply(client.clone(), obj.clone(), operator_name)
            .await;

        if let Err(Error::KubeError(_, cause)) = &result
            && let kube::Error::Api(ae) = cause.as_ref()
            && ae.code == 422
            && is_immutable_update_message(&ae.message)
        {
            info!(resource.name = &name, resource.namespace = &namespace, reason = %ae.reason, api_message = %ae.message, "recreating {} because an immutable field changed",
                    short_type_name::<K>().unwrap_or("Unknown"));
            self.kube_delete(client.clone(), metrics, &obj).await?;
            metrics.reconcile_deploy_delete_create_inc(
                short_type_name::<K>().unwrap_or("Unknown"),
                "immutable_api_error",
            );
            return self.kube_apply(client, obj, operator_name).await;
        }

        if let Err(Error::KubeError(_, cause)) = &result
            && let kube::Error::Api(ae) = cause.as_ref()
            && ae.code == 422
        {
            debug!(resource.kind = short_type_name::<K>().unwrap_or("Unknown"), resource.name = &name, resource.namespace = &namespace, reason = %ae.reason, api_message = %ae.message, "server-side apply rejected resource with a non-immutable 422; preserving the existing resource");
        }

        result
    }
}

#[cfg(test)]
mod test {
    use super::{Context, is_immutable_update_message};

    use crate::controller::State;
    use crate::controller::kanidm::{KanidmKey, KanidmUser};
    use crate::kanidm::crd::Kanidm;

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::FutureExt;
    use http::{Request, Response};
    use kanidm_client::{KanidmClient, KanidmClientBuilder};
    use kube::runtime::reflector::store::Writer;
    use kube::{Client, ResourceExt, client::Body};
    use opentelemetry::metrics::MeterProvider;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

    /// The `X-KANIDM-VERSION` the client expects. A debug-build client exits the process on a
    /// mismatch, so keep this equal to the `kanidm_client` version in the workspace.
    const KANIDM_VERSION: &str = "1.11.2";

    fn test_context() -> (Context<Kanidm>, ApiServerHandle) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
        let metrics = crate::metrics::Metrics::new(&provider.meter("test"), &["test"]);
        let state = State::new(
            metrics,
            Writer::default().as_reader(),
            Writer::default().as_reader(),
            None,
        );
        (
            state.to_context(Client::new(mock_service, "default"), "test"),
            handle,
        )
    }

    fn kanidm_key(kanidm: &Kanidm) -> KanidmKey {
        KanidmKey {
            namespace: kanidm.namespace().unwrap(),
            name: kanidm.name_any(),
        }
    }

    /// Start a fake Kanidm server answering every request with `status`. Returns a client
    /// pointing to it and the request lines it received.
    async fn start_fake_kanidm(status: u16) -> (Arc<KanidmClient>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let n = stream.read(&mut buffer).await.unwrap_or(0);
                if let Some(line) = String::from_utf8_lossy(&buffer[..n]).lines().next() {
                    seen.lock().unwrap().push(line.to_string());
                }
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-kanidm-version: {KANIDM_VERSION}\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnull"
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        let client = KanidmClientBuilder::new()
            .no_proxy()
            .address(format!("http://{address}"))
            .build()
            .unwrap();
        (Arc::new(client), requests)
    }

    #[tokio::test]
    async fn kanidm_cluster_client_reuses_valid_cached_session() {
        let (ctx, mut api_server) = test_context();
        let kanidm = Kanidm::test();
        let (cached, requests) = start_fake_kanidm(200).await;
        ctx.system_clients
            .write()
            .await
            .insert(kanidm_key(&kanidm), cached.clone());

        // Creating a new client would block on the Kubernetes API, which never answers here.
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            ctx.get_kanidm_cluster_client(&kanidm, KanidmUser::Admin),
        )
        .await
        .expect("cached client is reused without creating a new session")
        .expect("valid cached client");

        assert!(Arc::ptr_eq(&client, &cached));
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["GET /v1/auth/valid HTTP/1.1".to_string()]
        );
        assert!(api_server.next_request().now_or_never().is_none());

        ctx.release_kanidm_clients(&kanidm).await;
        assert!(
            ctx.system_clients
                .read()
                .await
                .get(&kanidm_key(&kanidm))
                .is_none()
        );
    }

    #[tokio::test]
    async fn kanidm_cluster_client_recreates_invalid_cached_session() {
        let (ctx, mut api_server) = test_context();
        let kanidm = Kanidm::test();
        let (stale, requests) = start_fake_kanidm(401).await;
        ctx.idm_clients
            .write()
            .await
            .insert(kanidm_key(&kanidm), stale);

        let api_server = async move {
            let (request, send) = api_server
                .next_request()
                .await
                .expect("Kanidm is read to create a new client");
            assert_eq!(
                request.uri().path(),
                "/apis/kaniop.rs/v1beta1/namespaces/default/kanidms/test"
            );
            send.send_response(
                Response::builder()
                    .status(404)
                    .body(Body::from(
                        br#"{"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404}"#
                            .to_vec(),
                    ))
                    .unwrap(),
            );
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                ctx.get_kanidm_cluster_client(&kanidm, KanidmUser::IdmAdmin),
                api_server
            )
        })
        .await
        .expect("invalid cached session falls back to client creation");

        assert!(result.is_err());
        // Validated once on the fast path and once more under the creation lock.
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["GET /v1/auth/valid HTTP/1.1".to_string(); 2]
        );
    }

    #[test]
    fn immutable_update_messages_are_classified() {
        assert!(is_immutable_update_message(
            "Deployment.apps \"example\" is invalid: spec.selector: Invalid value: v1.LabelSelector{}: field is immutable"
        ));
        assert!(is_immutable_update_message(
            "Service \"example\" is invalid: spec.clusterIPs[0]: Invalid value: \"10.0.0.1\": may not change once set"
        ));
        assert!(is_immutable_update_message(
            "StatefulSet.apps \"example\" is invalid: spec: Forbidden: updates to statefulset spec for fields other than 'replicas', 'ordinals', 'template', 'updateStrategy', 'revisionHistoryLimit', 'persistentVolumeClaimRetentionPolicy' and 'minReadySeconds' are forbidden"
        ));
    }

    #[test]
    fn ordinary_validation_messages_are_not_classified_as_immutable() {
        assert!(!is_immutable_update_message(
            "StatefulSet.apps \"example\" is invalid: spec.minReadySeconds: Invalid value: -1: must be greater than or equal to 0"
        ));
        assert!(!is_immutable_update_message(
            "StatefulSet.apps \"example\" is invalid: spec.template.spec.containers[0].image: Required value"
        ));
    }
}
