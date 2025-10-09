use super::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::kanidm::crd::Kanidm;

use std::collections::BTreeSet;
use std::ops::Not;

use chrono::Utc;
use k8s_openapi::api::core::v1::{Event, Secret};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::{conditions, wait::Condition},
};
use serde_json::json;

const KANIDM_NAME: &str = "test-oauth2";

fn check_oauth2_condition(cond: &str, status: String) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|oauth2| oauth2.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
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

fn is_oauth2_ready() -> impl Condition<KanidmOAuth2Client> {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|group| group.status.as_ref())
            .is_some_and(|status| status.ready)
    }
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
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Public cannot be changed.")
    );
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
    let oauth2_uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={oauth2_uid}"
    ));
    let event_api = Api::<Event>::namespaced(client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    assert!(
        event_list
            .items
            .iter()
            .any(|e| e.reason == Some("KanidmClientError".to_string()))
    );

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
async fn oauth2_secret() {
    let name = "test-secret";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api
        .get(&format!("{name}-kanidm-oauth2-credentials"))
        .await
        .unwrap();
    assert_eq!(secret.data.clone().unwrap().len(), 2);
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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

    oauth2.spec.redirect_url = [format!("https://{name}.updated.com/oauth2/callback")]
        .into_iter()
        .collect();
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

    oauth2.spec.redirect_url = [
        format!("https://{name}.updated.com/oauth2/callback"),
        "app://localhost".to_string(),
    ]
    .into_iter()
    .collect();
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_redirect_url_multiple_urls = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_redirect_url_multiple_urls
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_origin")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!("https://{name}.updated.com/oauth2/callback"),
            &"app://localhost".to_string(),
        ])
    );
}

async fn create_group(name: &str, client: Client) {
    let group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "mail": [format!("{name}@example.com")],
    });
    let group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();
}

#[tokio::test]
async fn oauth2_scope_map() {
    let name = "test-oauth2-scope-map";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let group_1 = "test-oauth2-scope-map-group-1";
    create_group(group_1, s.client.clone()).await;

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "scopeMap": [{
            "group": group_1,
            "scopes": ["scope1", "scope2"],
        }],
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
    wait_for(oauth2_api.clone(), name, is_oauth2("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_created = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_scope_map")
            .unwrap(),
        &[format!(
            r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#
        )]
    );

    let group_2 = "test-oauth2-scope-map-group-2";
    create_group(group_2, s.client.clone()).await;
    oauth2.spec.scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }, {
        "group": group_2,
        "scopes": ["scope3", "scope4"],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
    let oauth2_scope_map_added = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_scope_map_added
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_scope_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#),
            &format!(r#"{group_2}@{KANIDM_NAME}.localhost: {{"scope3", "scope4"}}"#)
        ])
    );

    oauth2.spec.scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }, {
        "group": group_2,
        "scopes": ["new_scope1", "new_scope2"],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_scope_map_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_scope_map_updated
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_scope_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#),
            &format!(r#"{group_2}@{KANIDM_NAME}.localhost: {{"new_scope1", "new_scope2"}}"#)
        ])
    );

    oauth2.spec.scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_scope_map_removed = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_scope_map_removed
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_scope_map")
            .unwrap(),
        &[format!(
            r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#
        ),]
    );

    oauth2.spec.scope_map = Some(BTreeSet::new());
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_scope_map_deleted = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert!(
        oauth2_scope_map_deleted
            .clone()
            .unwrap()
            .attrs
            .contains_key("oauth2_rs_scope_map")
            .not(),
    );
}

#[tokio::test]
async fn oauth2_scope_map_group_by_uuid_not_allowed() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "scopeMap": [{
                "group": "00000000-0000-0000-0000-000000000000",
                "scopes": ["scope1", "scope2"],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be name or SPN in Scope Maps.")
    );
}

#[tokio::test]
async fn oauth2_scope_map_unique_groups() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "scopeMap": [{
                "group": "group1",
                "scopes": ["scope1", "scope2"],
            },
            {
                "group": "group1",
                "scopes": ["scope3", "scope4"],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be unique in Scope Maps.")
    );
}

#[tokio::test]
async fn oauth2_sup_scope_map() {
    let name = "test-oauth2-sup-scope-map";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let group_1 = "test-oauth2-sup-scope-map-group-1";
    create_group(group_1, s.client.clone()).await;

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "supScopeMap": [{
            "group": group_1,
            "scopes": ["scope1", "scope2"],
        }],
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
    wait_for(oauth2_api.clone(), name, is_oauth2("SupScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_created = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_sup_scope_map")
            .unwrap(),
        &[format!(
            r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#
        )]
    );

    let group_2 = "test-oauth2-sup-scope-map-group-2";
    create_group(group_2, s.client.clone()).await;
    oauth2.spec.sup_scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }, {
        "group": group_2,
        "scopes": ["scope3", "scope4"],
    }]))
    .unwrap();
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
        is_oauth2_false("SupScopeMapUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SupScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
    let oauth2_scope_map_added = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_scope_map_added
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_sup_scope_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#),
            &format!(r#"{group_2}@{KANIDM_NAME}.localhost: {{"scope3", "scope4"}}"#)
        ])
    );

    oauth2.spec.sup_scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }, {
        "group": group_2,
        "scopes": ["new_scope1", "new_scope2"],
    }]))
    .unwrap();
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
        is_oauth2_false("SupScopeMapUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SupScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_sup_scope_map_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_sup_scope_map_updated
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_sup_scope_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#),
            &format!(r#"{group_2}@{KANIDM_NAME}.localhost: {{"new_scope1", "new_scope2"}}"#)
        ])
    );

    oauth2.spec.sup_scope_map = serde_json::from_value(json!([{
        "group": group_1,
        "scopes": ["scope1", "scope2"],
    }]))
    .unwrap();
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
        is_oauth2_false("SupScopeMapUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SupScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_sup_scope_map_removed = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_sup_scope_map_removed
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_sup_scope_map")
            .unwrap(),
        &[format!(
            r#"{group_1}@{KANIDM_NAME}.localhost: {{"scope1", "scope2"}}"#
        ),]
    );

    oauth2.spec.sup_scope_map = Some(BTreeSet::new());
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
        is_oauth2_false("SupScopeMapUpdated"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SupScopeMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_sup_scope_map_deleted = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert!(
        oauth2_sup_scope_map_deleted
            .clone()
            .unwrap()
            .attrs
            .contains_key("oauth2_rs_sup_scope_map")
            .not(),
    );
}

#[tokio::test]
async fn oauth2_sup_scope_map_group_by_uuid_not_allowed() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "supScopeMap": [{
                "group": "00000000-0000-0000-0000-000000000000",
                "scopes": ["scope1", "scope2"],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be name or SPN in Supplementary Scope Maps.")
    );
}

#[tokio::test]
async fn oauth2_sup_scope_map_unique_groups() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "supScopeMap": [{
                "group": "group1",
                "scopes": ["scope1", "scope2"],
            },
            {
                "group": "group1",
                "scopes": ["scope3", "scope4"],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be unique in Supplementary Scope Maps.")
    );
}

#[tokio::test]
async fn oauth2_claim_map() {
    let name = "test-oauth2-claim-map";
    let s = setup_kanidm_connection(KANIDM_NAME).await;
    let group_1 = "test-oauth2-claim-map-group-1";
    create_group(group_1, s.client.clone()).await;

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "displayname": "Oauth2 Update",
        "redirectUrl": [],
        "claimMap": [{
            "name": "test",
            "valuesMap": [{
                "group": group_1,
                "values": ["value1", "value2"],
            }],
        }],
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
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_created = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_created
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_claim_map")
            .unwrap(),
        &[format!(
            r#"test:{group_1}@{KANIDM_NAME}.localhost:;:"value1,value2""#
        )]
    );

    let group_2 = "test-oauth2-claim-map-group-2";
    create_group(group_2, s.client.clone()).await;
    oauth2.spec.claim_map = serde_json::from_value(json!([{
        "name": "test",
        "valuesMap": [{
            "group": group_1,
            "values": ["value1", "value2"],
        },
        {
            "group": group_2,
            "values": ["value3", "value4"],
        }
        ],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
    let oauth2_claim_map_added = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_claim_map_added
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_claim_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"test:{group_1}@{KANIDM_NAME}.localhost:;:"value1,value2""#),
            &format!(r#"test:{group_2}@{KANIDM_NAME}.localhost:;:"value3,value4""#),
        ])
    );

    oauth2.spec.claim_map = serde_json::from_value(json!([{
        "name": "test",
        "valuesMap": [{
            "group": group_1,
            "values": ["value1", "value2"],
        },
        {
            "group": group_2,
            "values": ["new_value1", "new_value2"],
        }
        ],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_claim_map_updated = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_claim_map_updated
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_claim_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"test:{group_1}@{KANIDM_NAME}.localhost:;:"value1,value2""#),
            &format!(r#"test:{group_2}@{KANIDM_NAME}.localhost:;:"new_value1,new_value2""#),
        ])
    );

    oauth2.spec.claim_map = serde_json::from_value(json!([
        {
            "name": "test",
            "valuesMap": [{
                "group": group_1,
                "values": ["value1", "value2"],
            },
            {
                "group": group_2,
                "values": ["new_value1", "new_value2"],
            }
            ],
        },
        {
            "name": "test2",
            "valuesMap": [{
                "group": group_1,
                "values": ["value1", "value2"],
            },
            {
                "group": group_2,
                "values": ["new_value1", "new_value2"],
            }
            ],
            "joinStrategy": "csv",
        }
    ]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_claim_map_multiple = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_claim_map_multiple
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_claim_map")
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &format!(r#"test:{group_1}@{KANIDM_NAME}.localhost:;:"value1,value2""#),
            &format!(r#"test:{group_2}@{KANIDM_NAME}.localhost:;:"new_value1,new_value2""#),
            &format!(r#"test2:{group_1}@{KANIDM_NAME}.localhost:,:"value1,value2""#),
            &format!(r#"test2:{group_2}@{KANIDM_NAME}.localhost:,:"new_value1,new_value2""#),
        ])
    );

    oauth2.spec.claim_map = serde_json::from_value(json!([{
        "name": "test",
        "valuesMap": [{
            "group": group_1,
            "values": ["value1", "value2"],
        }],
    }]))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_claim_map_removed = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert_eq!(
        oauth2_claim_map_removed
            .clone()
            .unwrap()
            .attrs
            .get("oauth2_rs_claim_map")
            .unwrap(),
        &[format!(
            r#"test:{group_1}@{KANIDM_NAME}.localhost:;:"value1,value2""#
        ),]
    );

    oauth2.spec.claim_map = Some(BTreeSet::new());
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2_false("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("ClaimMapUpdated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let oauth2_claim_map_deleted = s.kanidm_client.idm_oauth2_rs_get(name).await.unwrap();
    assert!(
        oauth2_claim_map_deleted
            .clone()
            .unwrap()
            .attrs
            .contains_key("oauth2_rs_claim_map")
            .not()
    );
}

#[tokio::test]
async fn oauth2_claim_maps_group_by_uuid_not_allowed() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "claimMap": [{
                "name": "test",
                "valuesMap": [{
                    "group": "00000000-0000-0000-0000-000000000000",
                    "values": ["value1", "value2"],
                }],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    dbg!(&result);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be name or SPN in Claim Maps.")
    );
}

#[tokio::test]
async fn oauth2_claim_maps_unique_groups() {
    let client = Client::try_default().await.unwrap();

    let oauth2 = KanidmOAuth2Client::new(
        "test-oauth2-scope-map-group-by-uuid-not-allowed",
        serde_json::from_value(json!({
            "kanidmRef": {
                "name": "test",
            },
            "redirectUrl": [],
            "displayname": "Test OAuth2 Client",
            "origin": "https://example.com",
            "claimMap": [{
                "name": "test",
                "valuesMap": [{
                    "group": "group1",
                    "values": ["value1", "value2"],
                },
                {
                    "group": "group1",
                    "values": ["value3", "value4"],
                }],
            }],
        }))
        .unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    dbg!(&result);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Groups must be unique in Claim Maps.")
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
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
    assert!(
        oauth2_pkce_enabled
            .clone()
            .unwrap()
            .attrs
            .contains_key("oauth2_allow_insecure_client_disable_pkce")
            .not()
    );
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
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Public clients cannot disable PKCE.")
    );
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Just public clients can allow localhost redirect.")
    );
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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

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

#[tokio::test]
async fn oauth2_different_namespace() {
    let name = "test-different-namespace";
    let kanidm_name = "test-different-namespace-kanidm-oauth2";
    let s = setup_kanidm_connection(kanidm_name).await;
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(kanidm_name).await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
            "namespace": "default",
        },
        "displayname": "Test Different Namespace",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "kaniop");
    let oauth2_uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    let opts = ListParams::default().fields(&format!(
            "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={oauth2_uid}"
        ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "kaniop");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let token_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("ResourceNotWatched".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(token_events.len(), 1);
    assert!(token_events
        .first()
        .unwrap()
        .message
        .as_deref()
        .unwrap()
        .contains(
            "configure `oauth2ClientNamespaceSelector` on Kanidm resource to watch this namespace"
        ));

    kanidm.metadata =
        serde_json::from_value(json!({"name": kanidm_name, "namespace": "default"})).unwrap();
    kanidm.spec.oauth2_client_namespace_selector = serde_json::from_value(json!({})).unwrap();
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();
    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    oauth2_api.delete(name, &Default::default()).await.unwrap();
    wait_for(
        oauth2_api.clone(),
        name,
        conditions::is_deleted(&oauth2_uid),
    )
    .await;

    kanidm.spec.oauth2_client_namespace_selector = serde_json::from_value(json!({
        "matchLabels": {
            "watch-oauth2": "true"
        }
    }))
    .unwrap();
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    let namespace_api = Api::<k8s_openapi::api::core::v1::Namespace>::all(s.client.clone());
    let ns_label_patch = json!({
        "metadata": {
            "labels": {
                "watch-oauth2": "true"
            }
        }
    });
    namespace_api
        .patch(
            "kaniop",
            &PatchParams::apply("e2e-test"),
            &Patch::Merge(&ns_label_patch),
        )
        .await
        .unwrap();

    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("Exists")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("Updated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    kanidm.spec.oauth2_client_namespace_selector = None;
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();
}
