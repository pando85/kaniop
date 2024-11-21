use std::time::Duration;

use super::wait_for;

use crate::test::setup_kanidm_connection;

use chrono::Utc;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kaniop_kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;
use kube::api::DeleteParams;
use kube::{
    api::{Patch, PatchParams, PostParams},
    runtime::{conditions, wait::Condition},
    Api,
};
use kube::{Client, ResourceExt};
use serde_json::json;

const KANIDM_NAME: &str = "test-person";

fn check_person_condition(cond: &str, status: String) -> impl Condition<KanidmPersonAccount> + '_ {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .map_or(false, |conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

fn is_person(cond: &str) -> impl Condition<KanidmPersonAccount> + '_ {
    check_person_condition(cond, "True".to_string())
}

fn is_person_false(cond: &str) -> impl Condition<KanidmPersonAccount> + '_ {
    check_person_condition(cond, "False".to_string())
}

fn person_json(kanidm_name: &str) -> serde_json::Value {
    json!({
        "kanidmRef": {
            "name": kanidm_name,
        },
        "personAttributes": {
            "displayname": "Alice",
            "mail": ["alice@example.com"],
        },
    })
}

#[tokio::test]
async fn person_lifecycle() {
    let name = "test-person-lifecycle";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let person_spec = person_json(KANIDM_NAME);
    let mut person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Valid")).await;

    let person_created = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        person_created
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Alice"
    );
    assert_eq!(
        person_created.unwrap().attrs.get("mail").unwrap(),
        &["alice@example.com".to_string()]
    );

    // Update the person
    person.spec.person_attributes.displayname = "Bob".to_string();

    person_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&person),
        )
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    let updated_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        updated_person
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Bob"
    );
    assert_eq!(
        updated_person.unwrap().attrs.get("mail").unwrap(),
        &["alice@example.com".to_string()]
    );

    // External modification of the person - overwritten by the operator
    s.kanidm_client
        .idm_person_account_update(name, None, Some("changed_displayname"), None, None)
        .await
        .unwrap();
    person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    let external_updated_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        external_updated_person
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Bob"
    );
    assert_eq!(
        external_updated_person
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap(),
        &["alice@example.com".to_string()]
    );

    // External modification of the person - manually managed
    s.kanidm_client
        .idm_person_account_update(name, None, None, Some("bob"), None)
        .await
        .unwrap();
    let updated_person_uid = person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Updated")).await;
    let external_updated_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        external_updated_person
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Bob"
    );
    assert_eq!(
        external_updated_person
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap(),
        &["alice@example.com".to_string()]
    );
    assert_eq!(
        external_updated_person
            .clone()
            .unwrap()
            .attrs
            .get("legalname")
            .unwrap()
            .first()
            .unwrap(),
        "bob"
    );

    // Make the person invalid
    person.spec.person_attributes.account_expire =
        Some(Time(Utc::now() - chrono::Duration::days(1)));

    person_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&person),
        )
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_false("Valid")).await;

    let invalid_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(!invalid_person
        .clone()
        .unwrap()
        .attrs
        .get("account_expire")
        .unwrap()
        .is_empty());

    // Delete the person
    person_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        person_api.clone(),
        name,
        conditions::is_deleted(&updated_person_uid),
    )
    .await;

    let result = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn person_create_no_idm() {
    let name = "test-person-create-no-idm";
    let client = Client::try_default().await.unwrap();
    let person_spec = person_json(name);
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;
    let person_result = person_api.get(name).await.unwrap();
    assert!(person_result.status.is_none());
    // TODO: warning event that Kanidm client cannot be created or similar
}

#[tokio::test]
async fn person_delete_person_when_idm_no_longer_exists() {
    let name = "test-delete-person-when-idm-no-longer-exists";
    let kanidm_name = "test-delete-person-when-idm-no-idm";
    let s = setup_kanidm_connection(kanidm_name).await;

    let person_spec = person_json(kanidm_name);
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");

    let kanidm_uid = kanidm_api.get(kanidm_name).await.unwrap().uid().unwrap();
    kanidm_api
        .delete(kanidm_name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        kanidm_api.clone(),
        kanidm_name,
        conditions::is_deleted(&kanidm_uid),
    )
    .await;

    person_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    // TODO: warning event that Kanidm client cannot be created or similar
}
