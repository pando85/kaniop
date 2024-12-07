use crate::test::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use std::ops::Not;

use k8s_openapi::api::core::v1::Event;
use kaniop_oauth2::crd::KanidmOAuth2Client;

use kube::{
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::wait::Condition,
    Api, Client,
};
use serde_json::json;

const KANIDM_NAME: &str = "test-oauth2";

fn check_oauth2_condition(cond: &str, status: String) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|oauth2| oauth2.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .map_or(false, |conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

fn is_oauth2(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    check_oauth2_condition(cond, "True".to_string())
}

fn is_oauth2_false(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    check_oauth2_condition(cond, "False".to_string())
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

#[tokio::test]
async fn oauth2_create_no_idm() {
    let name = "test-oauth2-create-no-idm";
    let client = Client::try_default().await.unwrap();
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": name,
        },
        "displayname": "Test OAuth2 Client",
        "redirectUrl": [],
        "origin": "https://example.com",
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
    ));
    let event_api = Api::<Event>::namespaced(client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    assert!(event_list
        .items
        .iter()
        .any(|e| e.reason == Some("KanidmClientError".to_string())));

    let oauth2_result = oauth2_api.get(name).await.unwrap();
    assert!(oauth2_result.status.is_none());
}

#[tokio::test]
async fn oauth2_update() {
    let name = "test-oauth2-update";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    let oauth2_created = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Oauth2 Update"
    );

    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin_landing")
            .unwrap()
            .first()
            .unwrap(),
        &format!("https://{name}.example.com/")
    );

    oauth2.spec.displayname = "Changed Display Name".to_string();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;
    let oauth2_displayname_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_displayname_updated
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Changed Display Name"
    );

    oauth2.spec.origin = format!("https://{name}.updated.com");
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    let oauth2_origin_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_origin_updated
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin_landing")
            .unwrap()
            .first()
            .unwrap(),
        &format!("https://{name}.updated.com/")
    );
}

#[tokio::test]
async fn oauth2_redirect_url() {
    let name = "test-oauth2-redirect-url";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [format!("https://{name}.example.com/oauth2/callback")],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("RedirectUrlUpdated")).await;

    let oauth2_created = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin")
            .unwrap(),
        &[format!("https://{name}.example.com/oauth2/callback")]
    );

    oauth2.spec.redirect_url = vec![format!("https://{name}.updated.com/oauth2/callback")];
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("RedirectUrlUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("RedirectUrlUpdated")).await;
    let oauth2_redirect_url_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_redirect_url_updated
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin")
            .unwrap(),
        &[format!("https://{name}.updated.com/oauth2/callback")]
    );

    oauth2.spec.redirect_url = vec![
        format!("https://{name}.updated.com/oauth2/callback"),
        "app://localhost".to_string(),
    ];
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("RedirectUrlUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("RedirectUrlUpdated")).await;

    let oauth2_redirect_url_multiple_urls = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_redirect_url_multiple_urls
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin")
            .unwrap(),
        &[
            format!("https://{name}.updated.com/oauth2/callback"),
            "app://localhost".to_string(),
        ]
    );
}

#[tokio::test]
async fn oauth2_strict_redirect_url() {
    let name = "test-strict-redirect-url";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    oauth2.spec.strict_redirect_url = Some(false);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("StrictRedirectUrlUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("StrictRedirectUrlUpdated"),
    )
    .await;
    let oauth2_strict_redirect_url_disabled =
        s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_strict_redirect_url_disabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_strict_redirect_uri")
            .unwrap()
            .first()
            .unwrap(),
        "false"
    );

    oauth2.spec.strict_redirect_url = Some(true);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("StrictRedirectUrlUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("StrictRedirectUrlUpdated"),
    )
    .await;

    let oauth2_strict_redirect_url_enabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_strict_redirect_url_enabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_strict_redirect_uri")
            .unwrap()
            .first()
            .unwrap(),
        "true"
    );
}

#[tokio::test]
async fn oauth2_disable_pkce() {
    let name = "test-disable-pkce";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    oauth2.spec.allow_insecure_client_disable_pkce = Some(true);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("DisablePkceUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("DisablePkceUpdated")).await;
    let oauth2_pkce_disabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_pkce_disabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_allow_insecure_client_disable_pkce")
            .unwrap()
            .first()
            .unwrap(),
        "true"
    );

    oauth2.spec.allow_insecure_client_disable_pkce = Some(false);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("DisablePkceUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("DisablePkceUpdated")).await;

    let oauth2_pkce_enabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert!(oauth2_pkce_enabled
        .clone()
        .unwrap()
        .attrs
        .contains_key("oauth2_allow_insecure_client_disable_pkce")
        .not());
}

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
async fn oauth2_prefer_short_username() {
    let name = "test-prefer-short-username";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    oauth2.spec.prefer_short_username = Some(true);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("PreferShortNameUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("PreferShortNameUpdated"),
    )
    .await;
    let oauth2_short_name_enabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_short_name_enabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_prefer_short_username")
            .unwrap()
            .first()
            .unwrap(),
        "true"
    );

    oauth2.spec.prefer_short_username = Some(false);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("PreferShortNameUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("PreferShortNameUpdated"),
    )
    .await;

    let oauth2_short_name_disabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_short_name_disabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_prefer_short_username")
            .unwrap()
            .first()
            .unwrap(),
        "false"
    );
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
async fn oauth2_allow_localhost_redirect() {
    let name = "test-allow-localhost-redirect";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "public": true,
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    oauth2.spec.allow_localhost_redirect = Some(true);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("AllowLocalhostRedirectUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("AllowLocalhostRedirectUpdated"),
    )
    .await;
    let oauth2_localhost_redirect_enabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_localhost_redirect_enabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_allow_localhost_redirect")
            .unwrap()
            .first()
            .unwrap(),
        "true"
    );

    oauth2.spec.allow_localhost_redirect = Some(false);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("AllowLocalhostRedirectUpdated"),
    )
    .await;
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2("AllowLocalhostRedirectUpdated"),
    )
    .await;

    let oauth2_localhost_redirect_disabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_localhost_redirect_disabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_allow_localhost_redirect")
            .unwrap()
            .first()
            .unwrap(),
        "false"
    );
}

#[tokio::test]
async fn oauth2_legacy_crypto() {
    let name = "test-legacy-crypto";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    oauth2.spec.jwt_legacy_crypto_enable = Some(true);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("LegacyCryptoUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("LegacyCryptoUpdated")).await;
    let oauth2_legacy_crypto_enabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_legacy_crypto_enabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_jwt_legacy_crypto_enable")
            .unwrap()
            .first()
            .unwrap(),
        "true"
    );

    oauth2.spec.jwt_legacy_crypto_enable = Some(false);
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("LegacyCryptoUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("LegacyCryptoUpdated")).await;

    let oauth2_legacy_crypto_disabled = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_legacy_crypto_disabled
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_jwt_legacy_crypto_enable")
            .unwrap()
            .first()
            .unwrap(),
        "false"
    );
}
