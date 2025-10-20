mod group;
mod kanidm;
mod kanidm_ref;
mod oauth2;
mod person;
mod service_account;

use std::ops::Not;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use kaniop_k8s_util::types::short_type_name;
use kaniop_operator::kanidm::crd::Kanidm;

use backon::{ExponentialBuilder, Retryable};
use k8s_openapi::api::core::v1::{Event, Secret};
use kanidm::is_kanidm;
use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kube::api::ListParams;
use kube::{
    Api, Client,
    runtime::wait::{Condition, await_condition},
};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use rustls::crypto::aws_lc_rs::default_provider;
use std::sync::Once;

static INIT: Once = Once::new();

pub fn init_crypto_provider() {
    INIT.call_once(|| {
        default_provider().install_default().unwrap();
    });
}

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
    let result = timeout(
        Duration::from_secs(90),
        await_condition(api.clone(), name, condition),
    )
    .await;

    match result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            eprintln!(
                "Error waiting for {}/{name}: {e}",
                short_type_name::<K>().unwrap_or("Unknown resource")
            );
            panic!()
        }
        Err(_) => {
            eprintln!(
                "Timeout waiting for {}/{name} to match condition.",
                short_type_name::<K>().unwrap_or("Unknown resource"),
            );

            if let Ok(resource) = api.get(name).await {
                eprintln!("Current resource state:");
                eprintln!("{:#?}", resource);
            } else {
                eprintln!("Resource not found or cannot be retrieved");
            }

            let client = api.clone().into_client();
            let event_api: Api<Event> =
                Api::namespaced(client, api.namespace().unwrap_or("default"));
            let event_params = ListParams::default()
                .fields(&format!("involvedObject.name={name}"))
                .limit(10);

            if let Ok(events) = event_api.list(&event_params).await {
                if !events.items.is_empty() {
                    eprintln!("\n📢 Recent events:");
                    for event in events.items.iter().rev().take(5) {
                        eprintln!(
                            "  - [{}] {}: {}",
                            event.type_.as_deref().unwrap_or("?"),
                            event.reason.as_deref().unwrap_or("?"),
                            event.message.as_deref().unwrap_or("")
                        );
                    }
                } else {
                    eprintln!("No events found for this resource");
                }
            }

            panic!("Timeout waiting for condition")
        }
    }
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
    init_crypto_provider();
    let client = Client::try_default().await.unwrap();
    let kanidm_api = Api::<Kanidm>::namespaced(client.clone(), "default");
    let domain = format!("{kanidm_name}.localhost");
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
            let password_bytes = secret_data.get("IDM_ADMIN_PASSWORD").unwrap();
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
        .retry(ExponentialBuilder::default().with_max_times(5))
        .sleep(tokio::time::sleep)
        .await
        .unwrap();
    SetupKanidmConnection {
        kanidm_client,
        client,
    }
}
