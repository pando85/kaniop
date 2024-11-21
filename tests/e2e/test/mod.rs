mod kanidm;
mod person;

use std::time::Duration;

use backoff::future::retry;
use backoff::{Error, ExponentialBackoffBuilder};
use k8s_openapi::api::core::v1::Secret;
use kanidm::is_kanidm;
use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kaniop_k8s_util::types::short_type_name;

use kaniop_kanidm::crd::Kanidm;
use kube::{
    runtime::wait::{await_condition, Condition},
    Api, Client,
};
use serde_json::json;
use tokio::time::timeout;

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
        panic!(
            "timeout waiting for {}/{name}",
            short_type_name::<K>().unwrap_or("Unknown resource")
        )
    })
    .unwrap();
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
    let idm_admin_password = if kanidm_api.get(kanidm_name).await.is_ok() {
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
            Some(json!(
                {
                    "domain": domain,
                    "ingress": {
                        "annotations": {
                            "nginx.ingress.kubernetes.io/backend-protocol": "HTTPS",
                        }
                    }
                }
            )),
        )
        .await;
        s.idm_admin_password
    };
    let backoff = ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(Some(Duration::from_secs(5)))
        .build();

    let _ = retry(backoff, || async {
        kanidm_client
            .auth_simple_password("idm_admin", &idm_admin_password)
            .await
            .map_err(Error::transient)
    })
    .await;
    SetupKanidmConnection {
        kanidm_client,
        client,
    }
}
