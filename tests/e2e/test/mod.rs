#[allow(unused_macros)]
macro_rules! e2e_test {
    ($name:ident, $body:block) => {
        #[::core::prelude::rust_2024::test]
        fn $name() {
            ::std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(|| {
                    ::tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(async { $body })
                })
                .unwrap()
                .join()
                .unwrap();
        }
    };
    ($(#[$meta:meta])* $name:ident, $body:block) => {
        $(#[$meta])*
        #[::core::prelude::rust_2024::test]
        fn $name() {
            ::std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(|| {
                    ::tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(async { $body })
                })
                .unwrap()
                .join()
                .unwrap();
        }
    };
}

mod group;
mod kanidm;
mod kanidm_ref;
mod mail_sender;
mod oauth2;
mod oauth2_secret_key_aliases;
mod oauth2_secret_template;
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

const DEFAULT_E2E_WAIT_TIMEOUT_SECONDS: u64 = 180;
const DEFAULT_E2E_EVENT_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_E2E_EVENT_POLL_INTERVAL_MILLISECONDS: u64 = 1000;
const DEFAULT_E2E_POLL_TIMEOUT_SECONDS: u64 = 32;
const DEFAULT_E2E_POLL_STABILIZATION_SECONDS: u64 = 2;
const DEFAULT_E2E_POLL_INTERVAL_SECONDS: u64 = 1;
const DEFAULT_E2E_STABILIZATION_SECONDS: u64 = 2;
const DEFAULT_E2E_SECRET_ROTATION_SECONDS: u64 = 5;

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn wait_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_WAIT_TIMEOUT_SECONDS",
        DEFAULT_E2E_WAIT_TIMEOUT_SECONDS,
    ))
}

fn event_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_EVENT_TIMEOUT_SECONDS",
        DEFAULT_E2E_EVENT_TIMEOUT_SECONDS,
    ))
}

fn event_poll_interval() -> Duration {
    Duration::from_millis(env_u64(
        "E2E_EVENT_POLL_INTERVAL_MILLISECONDS",
        DEFAULT_E2E_EVENT_POLL_INTERVAL_MILLISECONDS,
    ))
}

fn poll_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_POLL_TIMEOUT_SECONDS",
        DEFAULT_E2E_POLL_TIMEOUT_SECONDS,
    ))
}

fn poll_stabilization() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_POLL_STABILIZATION_SECONDS",
        DEFAULT_E2E_POLL_STABILIZATION_SECONDS,
    ))
}

fn poll_interval() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_POLL_INTERVAL_SECONDS",
        DEFAULT_E2E_POLL_INTERVAL_SECONDS,
    ))
}

fn stabilization_delay() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_STABILIZATION_SECONDS",
        DEFAULT_E2E_STABILIZATION_SECONDS,
    ))
}

fn secret_rotation_delay() -> Duration {
    Duration::from_secs(env_u64(
        "E2E_SECRET_ROTATION_SECONDS",
        DEFAULT_E2E_SECRET_ROTATION_SECONDS,
    ))
}

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
    wait_for_result(api, name, condition).await.unwrap();
}

pub async fn wait_for_result<K, C>(api: Api<K>, name: &str, condition: C) -> Result<(), String>
where
    K: kube::Resource
        + Clone
        + std::fmt::Debug
        + for<'de> k8s_openapi::serde::Deserialize<'de>
        + 'static
        + Send,
    C: Condition<K>,
{
    if let Ok(resource) = api.get(name).await {
        if condition.matches_object(Some(&resource)) {
            return Ok(());
        }
    }

    let result = timeout(
        wait_timeout(),
        await_condition(api.clone(), name, condition),
    )
    .await;

    match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!(
            "Error waiting for {}/{name}: {e}",
            short_type_name::<K>().unwrap_or("Unknown resource")
        )),
        Err(_) => {
            let error_msg = format!(
                "Timeout waiting for {}/{name} to match condition.",
                short_type_name::<K>().unwrap_or("Unknown resource"),
            );
            eprintln!("{}", error_msg);

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
                    eprintln!("\nRecent events:");
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

            Err(error_msg)
        }
    }
}

pub async fn check_event_with_timeout(event_api: &Api<Event>, opts: &ListParams) {
    timeout(event_timeout(), async {
        loop {
            match event_api.list(opts).await {
                Ok(event_list) if event_list.items.is_empty().not() => {
                    return true;
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("error listing events with params {opts:?}: {error}");
                }
            }
            tokio::time::sleep(event_poll_interval()).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        eprintln!("timeout waiting for event with params: {opts:?}",);
        panic!()
    });
}

pub async fn poll_until<T, F, Fut>(description: &str, f: F) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
    T: std::fmt::Debug,
{
    poll_until_result(description, f)
        .await
        .unwrap_or_else(|e| panic!("{e}"))
}

pub async fn poll_until_result<T, F, Fut>(description: &str, f: F) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
    T: std::fmt::Debug,
{
    tokio::time::sleep(poll_stabilization()).await;

    let start = std::time::Instant::now();
    loop {
        if let Some(value) = f().await {
            return Ok(value);
        }

        if start.elapsed() > poll_timeout() {
            return Err(format!("Timeout waiting for: {description}"));
        }

        tokio::time::sleep(poll_interval()).await;
    }
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
            wait_for(kanidm_api.clone(), kanidm_name, is_kanidm("Available")).await;
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
        .retry(ExponentialBuilder::default().with_max_times(8))
        .sleep(tokio::time::sleep)
        .await
        .unwrap();
    SetupKanidmConnection {
        kanidm_client,
        client,
    }
}
