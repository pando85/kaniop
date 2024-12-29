use crate::test::kanidm::setup;

use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_person::crd::KanidmPersonAccount;

use kube::api::{Api, Patch, PatchParams, PostParams};
use serde_json::json;

#[tokio::test]
async fn kanidm_ref_immutable_group() {
    let name = "test-kanidm-ref-immutable-group";
    let s = setup(name, None).await;

    // Create a KanidmGroup with a kanidmRef
    let group_spec = json!({
        "kanidmRef": {
            "name": name,
        },
    });
    let group = KanidmGroup::new("test-group", serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    // Try to change the kanidmRef and expect it to fail
    let mut updated_group = group_api.get("test-group").await.unwrap();
    updated_group.spec.kanidm_ref.name = "different-kanidm".to_string();
    updated_group.metadata.managed_fields = None;

    let result = group_api
        .patch(
            "test-group",
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&updated_group),
        )
        .await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Value is immutable")
    );
}

#[tokio::test]
async fn kanidm_ref_immutable_person() {
    let name = "test-kanidm-ref-immutable-person";
    let s = setup(name, None).await;

    // Create a KanidmPersonAccount with a kanidmRef
    let person_spec = json!({
        "kanidmRef": {
            "name": name,
        },
        "personAttributes": {
            "displayname": "Test Person",
        },
    });
    let person =
        KanidmPersonAccount::new("test-person", serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    // Try to change the kanidmRef and expect it to fail
    let mut updated_person = person_api.get("test-person").await.unwrap();
    updated_person.spec.kanidm_ref.name = "different-kanidm".to_string();
    updated_person.metadata.managed_fields = None;

    let result = person_api
        .patch(
            "test-person",
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&updated_person),
        )
        .await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Value is immutable")
    );
}

#[tokio::test]
async fn kanidm_ref_immutable_oauth2() {
    let name = "test-kanidm-ref-immutable-oauth2";
    let s = setup(name, None).await;

    // Create a KanidmOAuth2Client with a kanidmRef
    let oauth2_spec = json!({
        "kanidmRef": {
            "name": name,
        },
        "displayname": "Test OAuth2 Client",
        "origin": "https://example.com",
        "redirectUrl": ["https://example.com/callback"],
    });
    let oauth2 =
        KanidmOAuth2Client::new("test-oauth2", serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    // Try to change the kanidmRef and expect it to fail
    let mut updated_oauth2 = oauth2_api.get("test-oauth2").await.unwrap();
    updated_oauth2.spec.kanidm_ref.name = "different-kanidm".to_string();
    updated_oauth2.metadata.managed_fields = None;

    let result = oauth2_api
        .patch(
            "test-oauth2",
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&updated_oauth2),
        )
        .await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Value is immutable")
    );
}
