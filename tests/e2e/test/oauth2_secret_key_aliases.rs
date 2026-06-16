use super::{setup_kanidm_connection, wait_for};

use kaniop_oauth2::crd::KanidmOAuth2Client;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api,
    api::{Patch, PatchParams, PostParams},
    runtime::wait::Condition,
};
use serde_json::json;

const KANIDM_NAME: &str = "test-oauth2";

fn is_oauth2(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|o| o.status.as_ref())
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conds| conds.iter().any(|c| c.type_ == cond && c.status == "True"))
    }
}

fn is_oauth2_ready() -> impl Condition<KanidmOAuth2Client> {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|o| o.status.as_ref()).is_some_and(|s| s.ready)
    }
}

e2e_test!(oauth2_secret_key_aliases_basic, {
    let name = "test-ska-basic";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Basic Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretKeyAliases": {
            "clientId": ["client-id"],
            "clientSecret": ["client-secret"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let data = secret.data.unwrap();

    assert!(
        data.contains_key("CLIENT_ID"),
        "canonical CLIENT_ID must be present"
    );
    assert!(
        data.contains_key("CLIENT_SECRET"),
        "canonical CLIENT_SECRET must be present"
    );
    assert!(
        data.contains_key("client-id"),
        "clientId alias must be present"
    );
    assert!(
        data.contains_key("client-secret"),
        "clientSecret alias must be present"
    );
});

e2e_test!(oauth2_secret_key_aliases_multiple, {
    let name = "test-ska-multiple";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Multiple Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretKeyAliases": {
            "clientId": ["client-id", "oauth-client-id"],
            "clientSecret": ["client-secret", "oauth-client-secret"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let data = secret.data.unwrap();

    assert!(
        data.contains_key("CLIENT_ID"),
        "canonical CLIENT_ID must be present"
    );
    assert!(
        data.contains_key("CLIENT_SECRET"),
        "canonical CLIENT_SECRET must be present"
    );
    assert!(
        data.contains_key("client-id"),
        "clientId alias 1 must be present"
    );
    assert!(
        data.contains_key("oauth-client-id"),
        "clientId alias 2 must be present"
    );
    assert!(
        data.contains_key("client-secret"),
        "clientSecret alias 1 must be present"
    );
    assert!(
        data.contains_key("oauth-client-secret"),
        "clientSecret alias 2 must be present"
    );
});

e2e_test!(oauth2_secret_key_aliases_dedup, {
    let name = "test-ska-dedup";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Dedup Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretKeyAliases": {
            "clientId": ["client-id", "client-id"],
            "clientSecret": ["client-secret"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let data = secret.data.unwrap();

    assert!(
        data.contains_key("client-id"),
        "clientId alias must be present"
    );
    assert_eq!(
        data.len(),
        4,
        "duplicate aliases should be deduplicated (CLIENT_ID, CLIENT_SECRET, client-id, client-secret)"
    );
});

e2e_test!(oauth2_secret_key_aliases_rotation_preserves_aliases, {
    let name = "test-ska-rotation";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Rotation Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretRotation": {
            "enabled": true,
            "periodDays": 90
        },
        "secretKeyAliases": {
            "clientId": ["client-id"],
            "clientSecret": ["client-secret"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretRotated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret_before = secret_api.get(&secret_name).await.unwrap();
    let data_before = secret_before.data.unwrap();

    assert!(
        data_before.contains_key("client-id"),
        "clientId alias must be present before rotation"
    );
    assert!(
        data_before.contains_key("client-secret"),
        "clientSecret alias must be present before rotation"
    );

    let client_secret_before = data_before.get("CLIENT_SECRET").unwrap();

    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "metadata": {
                    "annotations": {
                        "kaniop.rs/force-secret-rotation": Timestamp::now().to_string()
                    }
                }
            })),
        )
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretRotated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_after = secret_api.get(&secret_name).await.unwrap();
    let data_after = secret_after.data.unwrap();

    assert!(
        data_after.contains_key("client-id"),
        "clientId alias must survive rotation"
    );
    assert!(
        data_after.contains_key("client-secret"),
        "clientSecret alias must survive rotation"
    );
    assert!(
        data_after.contains_key("CLIENT_ID"),
        "canonical CLIENT_ID must survive rotation"
    );
    assert!(
        data_after.contains_key("CLIENT_SECRET"),
        "canonical CLIENT_SECRET must survive rotation"
    );

    let client_secret_after = data_after.get("CLIENT_SECRET").unwrap();
    assert_ne!(
        client_secret_before, client_secret_after,
        "CLIENT_SECRET should have changed after rotation"
    );
});

e2e_test!(oauth2_secret_key_aliases_add_after_creation, {
    let name = "test-ska-add-after";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Add After Creation Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret_before = secret_api.get(&secret_name).await.unwrap();
    let data_before = secret_before.data.unwrap();
    assert_eq!(
        data_before.len(),
        2,
        "Secret should only have canonical keys before aliases are added"
    );
    assert!(
        !data_before.contains_key("client-id"),
        "clientId alias should not be present before adding aliases"
    );

    oauth2.spec.secret_key_aliases = serde_json::from_value(json!({
        "clientId": ["client-id"],
        "clientSecret": ["client-secret"]
    }))
    .unwrap();
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_after = secret_api.get(&secret_name).await.unwrap();
    let data_after = secret_after.data.unwrap();
    assert!(
        data_after.contains_key("client-id"),
        "clientId alias should be present after adding aliases"
    );
    assert!(
        data_after.contains_key("client-secret"),
        "clientSecret alias should be present after adding aliases"
    );
});

e2e_test!(oauth2_secret_key_aliases_remove, {
    let name = "test-ska-remove";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "SKA Remove Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretKeyAliases": {
            "clientId": ["client-id"],
            "clientSecret": ["client-secret"]
        }
    });
    let mut oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret_before = secret_api.get(&secret_name).await.unwrap();
    let data_before = secret_before.data.unwrap();
    assert!(
        data_before.contains_key("client-id"),
        "clientId alias should be present initially"
    );

    oauth2.spec.secret_key_aliases = None;
    oauth2_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&oauth2),
        )
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let secret_after = secret_api.get(&secret_name).await.unwrap();
    let data_after = secret_after.data.unwrap();
    assert!(
        !data_after.contains_key("client-id"),
        "clientId alias should be removed"
    );
    assert!(
        !data_after.contains_key("client-secret"),
        "clientSecret alias should be removed"
    );
    assert!(
        data_after.contains_key("CLIENT_ID"),
        "canonical CLIENT_ID must remain"
    );
    assert!(
        data_after.contains_key("CLIENT_SECRET"),
        "canonical CLIENT_SECRET must remain"
    );
});

e2e_test!(oauth2_secret_key_aliases_public_client_rejected, {
    let client = kube::Client::try_default().await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {"name": "test"},
        "displayname": "SKA Public Client Test",
        "redirectUrl": [],
        "origin": "https://example.com",
        "public": true,
        "secretKeyAliases": {
            "clientId": ["client-id"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(
        "test-ska-public-rejected",
        serde_json::from_value(oauth2_spec).unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Public clients cannot have secret key aliases")
    );
});

e2e_test!(oauth2_secret_key_aliases_invalid_key_name_rejected, {
    let client = kube::Client::try_default().await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {"name": "test"},
        "displayname": "SKA Invalid Key Name Test",
        "redirectUrl": [],
        "origin": "https://example.com",
        "secretKeyAliases": {
            "clientId": ["client-id/invalid"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(
        "test-ska-invalid-key-rejected",
        serde_json::from_value(oauth2_spec).unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("clientId aliases must be valid Kubernetes Secret key names")
    );
});

e2e_test!(oauth2_secret_key_aliases_canonical_key_rejected, {
    let client = kube::Client::try_default().await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {"name": "test"},
        "displayname": "SKA Canonical Key Rejected Test",
        "redirectUrl": [],
        "origin": "https://example.com",
        "secretKeyAliases": {
            "clientId": ["CLIENT_ID"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(
        "test-ska-canonical-rejected",
        serde_json::from_value(oauth2_spec).unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("clientId aliases cannot use the canonical key name CLIENT_ID")
    );
});

e2e_test!(oauth2_secret_key_aliases_overlap_rejected, {
    let client = kube::Client::try_default().await.unwrap();

    let oauth2_spec = json!({
        "kanidmRef": {"name": "test"},
        "displayname": "SKA Overlap Rejected Test",
        "redirectUrl": [],
        "origin": "https://example.com",
        "secretKeyAliases": {
            "clientId": ["shared-key"],
            "clientSecret": ["shared-key"]
        }
    });
    let oauth2 = KanidmOAuth2Client::new(
        "test-ska-overlap-rejected",
        serde_json::from_value(oauth2_spec).unwrap(),
    );
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(client.clone(), "default");
    let result = oauth2_api.create(&PostParams::default(), &oauth2).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("clientId and clientSecret aliases cannot overlap")
    );
});
