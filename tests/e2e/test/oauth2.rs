use crate::test::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_oauth2::crd::KanidmOAuth2Client;

use kube::{
    api::{Patch, PatchParams, PostParams},
    Api, Client,
};
use serde_json::json;

const KANIDM_NAME: &str = "test-oauth2";

#[tokio::test]
async fn oauth2_disable_pkce_in_public_clients() {
    let client = Client::try_default().await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": "test",
        },
        "redirectUrl": [],
        "displayname": "Test OAuth2 Client",
        "origin": "https://example.com",
        "public": true,
        "allowInsecureClientDisablePkce": true,
    });
    let oauth2 = KanidmOAuth2Client::new(
        "test-disable-pkce-in-public-clients",
        serde_json::from_value(oauth2_spec).unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Public clients cannot disable PKCE."));
}

#[tokio::test]
async fn oauth2_non_public_client_allow_localhost_redirect() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-non-public-client-allow-localhost-redirect",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "public": false,
            "allowLocalhostRedirect": true,
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Just public clients can allow localhost redirect."));
}

#[tokio::test]
async fn oauth2_change_public() {
    let name = "test-change-oauth2-public";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "redirectUrl": [],
        "displayname": "Test OAuth2 Client",
        "origin": "https://example.com",
        "public": false,
    });

    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();
    oauth2.spec.public = true;
    let result = oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await;

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Public cannot be changed."));
}
