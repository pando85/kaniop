use crate::crd::KanidmPersonAccount;
use crate::reconcile::reconcile_person_account;

use kanidm_client::KanidmClient;
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{
    ControllerId, DEFAULT_RECONCILE_INTERVAL, State, check_api_queryable,
    context::{BackoffContext, Context as KaniopContext, IdmClientContext},
};
use kaniop_operator::error::{Error, Result};
use kaniop_operator::metrics::ControllerMetrics;

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher;
use time::OffsetDateTime;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::{info, trace};

pub const CONTROLLER_ID: ControllerId = "person-account";

#[derive(Clone)]
pub struct Context {
    pub kaniop_ctx: KaniopContext<KanidmPersonAccount>,
    /// Internal controller cache
    // TODO: use this just in person account controller. Is UID better than ObjectRef?
    pub internal_cache: Arc<RwLock<HashMap<ObjectRef<KanidmPersonAccount>, time::OffsetDateTime>>>,
}

impl Context {
    pub fn new(kaniop_ctx: KaniopContext<KanidmPersonAccount>) -> Self {
        Context {
            kaniop_ctx,
            internal_cache: Arc::default(),
        }
    }
}

impl BackoffContext<KanidmPersonAccount> for Context {
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        self.kaniop_ctx.metrics()
    }
    async fn get_backoff(&self, obj_ref: ObjectRef<KanidmPersonAccount>) -> Duration {
        self.kaniop_ctx.get_backoff(obj_ref).await
    }

    async fn reset_backoff(&self, obj_ref: ObjectRef<KanidmPersonAccount>) {
        self.kaniop_ctx.reset_backoff(obj_ref).await
    }
}

impl IdmClientContext<KanidmPersonAccount> for Context {
    async fn get_idm_client(&self, obj: &KanidmPersonAccount) -> Result<Arc<KanidmClient>> {
        self.kaniop_ctx.get_idm_client(obj).await
    }
}

pub async fn cleanup_expired_tokens(ctx: Arc<Context>) {
    loop {
        tokio::time::sleep(DEFAULT_RECONCILE_INTERVAL).await;
        trace!("cleaning up expired tokens cache");
        let now = OffsetDateTime::now_utc();
        {
            let mut cache = ctx.internal_cache.write().await;
            cache.retain(|_, v| *v > now);
        }
    }
}

/// Initialize Kanidm controller and shared state
pub async fn run(state: State, client: Client) {
    let person = check_api_queryable::<KanidmPersonAccount>(client.clone()).await;

    let ctx = Arc::new(Context::new(state.to_context(client, CONTROLLER_ID)));

    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    // https://kubernetes.io/docs/reference/using-api/api-concepts/#streaming-lists
    let person_controller = Controller::new(person, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_person_account),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.kaniop_ctx.metrics.ready_set(1);
    tokio::select! {
        _ = person_controller => {},
        _ = cleanup_expired_tokens(ctx.clone()) => {},
    }
}
