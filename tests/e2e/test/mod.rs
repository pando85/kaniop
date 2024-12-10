mod group;
mod kanidm;
mod oauth2;
mod person;

use std::ops::Not;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use kaniop_k8s_util::types::short_type_name;
use kaniop_operator::crd::kanidm::Kanidm;

use backon::{ExponentialBuilder, Retryable};
use k8s_openapi::api::core::v1::{Event, Secret};
use kanidm::is_kanidm;
use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kube::api::ListParams;
use kube::{
    runtime::wait::{await_condition, Condition},
    Api, Client,
};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::time::timeout;

static KANIDM_SETUP_LOCK: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::const_new(1)));

pub async fn wait_for<K, C>(api: Api<K>, name: &str, condition: C)
where
    K: kube::Resource
        + Clone
        + std::fmt::Debug
        + for<'de> k8s_openapi::serde::Deserialize<'de>
        + 'static
        + Send,
    C: Condition<K>,
{
    timeout(
        Duration::from_secs(60),
        await_condition(api, name, condition),
    )
    .await
    .unwrap_or_else(|_| {
        eprintln!(
            "timeout waiting for {}/{name} to match condition",
            short_type_name::<K>().unwrap_or("Unknown resource")
        );
        panic!()
    })
    .unwrap();
}

pub async fn check_event_with_timeout(event_api: &Api<Event>, opts: &ListParams) {
    timeout(Duration::from_secs(10), async {
        loop {
            let event_list = event_api.list(opts).await.unwrap();
            if event_list.items.is_empty().not() {
                return true;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        eprintln!("timeout waiting for event with params: {opts:?}",);
        panic!()
    });
}

pub struct SetupKanidmConnection {
    pub kanidm_client: KanidmClient,
    pub client: Client,
}

// Return a Kanidm connection for the given name, creating it if it doesn't exist
pub async fn setup_kanidm_connection(kanidm_name: &str) -> SetupKanidmConnection {
    let client = Client::try_default().await.unwrap();
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let domain = format!("{}.localhost", kanidm_name);
    let kanidm_client = KanidmClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .address(format!("https://{domain}"))
        .connect_timeout(5)
        .build()
        .unwrap();

    let idm_admin_password = {
        let avoid_race_condition = KANIDM_SETUP_LOCK.acquire().await;

        if kanidm_api.get(kanidm_name).await.is_ok() {
            drop(avoid_race_condition);
            let secret_api = Api::<Secret>::namespaced(client.clone(), "default");
            wait_for(kanidm_api.clone(), kanidm_name, is_kanidm("Initialized")).await;
            let admin_secret = secret_api
                .get(&format!("{kanidm_name}-admin-passwords"))
                .await
                .unwrap();
            let secret_data = admin_secret.data.unwrap();
            let password_bytes = secret_data.get("idm_admin").unwrap();
            std::str::from_utf8(&password_bytes.0).unwrap().to_string()
        } else {
            let s = kanidm::setup(
                kanidm_name,
                Some(json!({
                    "domain": domain,
                    "ingress": {
                        "annotations": {
                            "nginx.ingress.kubernetes.io/backend-protocol": "HTTPS",
                        }
                    }
                })),
            )
            .await;
            s.idm_admin_password
        }
    };

    let retryable_future = || async {
        kanidm_client
            .auth_simple_password("idm_admin", &idm_admin_password)
            .await
    };

    retryable_future
        .retry(ExponentialBuilder::default().with_max_times(3))
        .sleep(tokio::time::sleep)
        .await
        .unwrap();
    SetupKanidmConnection {
        kanidm_client,
        client,
    }
}
