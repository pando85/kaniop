use crate::crd::KanidmOAuth2Client;
use crate::reconcile::reconcile_oauth2;

use kaniop_k8s_util::error::{Error, Result};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{
    ControllerId, State, check_api_queryable,
    context::{BackoffContext, Context as KaniopContext, IdmClientContext},
};
use kaniop_operator::controller::{
    RELOAD_BUFFER_SIZE, SUBSCRIBE_BUFFER_SIZE, create_subscriber, create_watcher,
};
use kaniop_operator::metrics::ControllerMetrics;

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc;
use k8s_openapi::api::core::v1::Secret;
use kanidm_client::KanidmClient;
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::watcher;
use tokio::time::Duration;
use tracing::info;

pub const CONTROLLER_ID: ControllerId = "oauth2";

#[derive(Clone)]
pub struct Context {
    pub kaniop_ctx: KaniopContext<KanidmOAuth2Client>,
    /// Secret store for OAuth2 clients
    pub secret_store: Store<Secret>,
}

impl Context {
    pub fn new(kaniop_ctx: KaniopContext<KanidmOAuth2Client>, secret_store: Store<Secret>) -> Self {
        Context {
            kaniop_ctx,
            secret_store,
        }
    }
}

impl BackoffContext<KanidmOAuth2Client> for Context {
    fn metrics(&self) -> &Arc<ControllerMetrics> {
        self.kaniop_ctx.metrics()
    }
    async fn get_backoff(&self, obj_ref: ObjectRef<KanidmOAuth2Client>) -> Duration {
        self.kaniop_ctx.get_backoff(obj_ref).await
    }

    async fn reset_backoff(&self, obj_ref: ObjectRef<KanidmOAuth2Client>) {
        self.kaniop_ctx.reset_backoff(obj_ref).await
    }
}

impl IdmClientContext<KanidmOAuth2Client> for Context {
    async fn get_idm_client(&self, obj: &KanidmOAuth2Client) -> Result<Arc<KanidmClient>> {
        self.kaniop_ctx.get_idm_client(obj).await
    }
}

/// Initialize Kanidm controller and shared state
pub async fn run(state: State, client: Client) {
    let oauth2 = check_api_queryable::<KanidmOAuth2Client>(client.clone()).await;
    let secret = check_api_queryable::<Secret>(client.clone()).await;
    let secret_r = create_subscriber::<Secret>(SUBSCRIBE_BUFFER_SIZE);

    let (reload_tx, reload_rx) = mpsc::channel(RELOAD_BUFFER_SIZE);
    let ctx = Arc::new(Context::new(
        state.to_context(client, CONTROLLER_ID),
        secret_r.store,
    ));
    let kaniop_ctx = Arc::new(ctx.kaniop_ctx.clone());

    // TODO: just metadata is needed
    let secret_watcher = create_watcher(
        secret,
        secret_r.writer,
        reload_tx.clone(),
        CONTROLLER_ID,
        kaniop_ctx,
    );
    info!(msg = format!("starting {CONTROLLER_ID} controller"));
    // TODO: watcher::Config::default().streaming_lists() when stabilized in K8s
    // https://kubernetes.io/docs/reference/using-api/api-concepts/#streaming-lists
    let oauth2_controller = Controller::new(oauth2, watcher::Config::default().any_semantic())
        // debounce to filter out reconcile calls that happen quick succession (only taking the latest)
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .owns_shared_stream(secret_r.subscriber)
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_oauth2),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.kaniop_ctx.metrics.ready_set(1);
    tokio::select! {
        _ = oauth2_controller => {},
        _ = secret_watcher => {},
    }
}
