use super::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_operator::crd::KanidmPersonPosixAttributes;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_person::crd::KanidmPersonAccount;

use std::ops::Not;

use chrono::Utc;
use k8s_openapi::api::core::v1::Event;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::api::DeleteParams;
use kube::{
    Api,
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::{conditions, wait::Condition},
};
use kube::{Client, ResourceExt};
use serde_json::json;

const KANIDM_NAME: &str = "test-person";

fn check_person_condition(cond: &str, status: String) -> impl Condition<KanidmPersonAccount> + '_ {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
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

fn is_person_ready() -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|group| group.status.as_ref())
            .is_some_and(|status| status.ready)
    }
}

#[tokio::test]
async fn person_lifecycle() {
    let name = "test-person-lifecycle";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Alice",
            "mail": ["alice@example.com"],
        },
    });
    let mut person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Valid")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

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
    wait_for(person_api.clone(), name, is_person_ready()).await;

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
    assert!(
        updated_person
            .clone()
            .unwrap()
            .attrs
            .contains_key("gidnumber")
            .not()
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
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

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
    // we modify the displayname to know that the operator modified the object
    s.kanidm_client
        .idm_person_account_update(name, None, Some("bob"), Some("bob"), None)
        .await
        .unwrap();
    person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
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

    // Add Posix attributes
    person.spec.posix_attributes = Some(KanidmPersonPosixAttributes {
        ..Default::default()
    });

    person_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&person),
        )
        .await
        .unwrap();
    wait_for(person_api.clone(), name, is_person("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
    let posix_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(
        posix_person
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    person.spec.posix_attributes = Some(KanidmPersonPosixAttributes {
        loginshell: Some("/bin/bash".to_string()),
        ..Default::default()
    });

    person_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&person),
        )
        .await
        .unwrap();
    wait_for(person_api.clone(), name, is_person_false("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
    let posix_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(
        posix_person
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );
    assert_eq!(
        posix_person
            .clone()
            .unwrap()
            .attrs
            .get("loginshell")
            .unwrap()
            .first()
            .unwrap(),
        "/bin/bash"
    );

    // External modification of posix - overwritten by the operator
    s.kanidm_client
        .idm_person_account_unix_extend(name, None, Some("/usr/bin/nologin"))
        .await
        .unwrap();
    person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
    let external_posix_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        external_posix_person
            .clone()
            .unwrap()
            .attrs
            .get("loginshell")
            .unwrap()
            .first()
            .unwrap(),
        "/bin/bash"
    );
    assert!(
        external_posix_person
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    // External modification of posix - manually managed
    // we modify the shell attribute to know that the operator modified the object
    s.kanidm_client
        .idm_person_account_unix_extend(name, Some(555555), Some("/usr/bin/nologin"))
        .await
        .unwrap();
    person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person("PosixUpdated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
    let external_posix_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert_eq!(
        external_posix_person
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .first()
            .unwrap(),
        "555555"
    );
    assert_eq!(
        external_posix_person
            .clone()
            .unwrap()
            .attrs
            .get("loginshell")
            .unwrap()
            .first()
            .unwrap(),
        "/bin/bash"
    );

    // Keep Posix attributes
    person.spec.posix_attributes = None;
    // we modify the displayname to know that the operator modified the object
    person.spec.person_attributes.displayname = "Alice".to_string();

    let posix_person_uid = person_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&person),
        )
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
    let posix_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(
        posix_person
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );
    assert_eq!(
        posix_person
            .clone()
            .unwrap()
            .attrs
            .get("loginshell")
            .unwrap()
            .first()
            .unwrap(),
        "/bin/bash"
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
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let invalid_person = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(
        !invalid_person
            .clone()
            .unwrap()
            .attrs
            .get("account_expire")
            .unwrap()
            .is_empty()
    );

    // Delete the person
    person_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        person_api.clone(),
        name,
        conditions::is_deleted(&posix_person_uid),
    )
    .await;

    let result = s.kanidm_client.idm_person_account_get(name).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn person_create_no_idm() {
    let name = "test-person-create-no-idm";
    let client = Client::try_default().await.unwrap();
    let person_spec = json!({
        "kanidmRef": {
            "name": name,
        },
        "personAttributes": {
            "displayname": "Test Person Create",
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
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

    let person_result = person_api.get(name).await.unwrap();
    assert!(person_result.status.is_none());
}

#[tokio::test]
async fn person_delete_person_when_idm_no_longer_exists() {
    let name = "test-delete-person-when-idm-no-longer-exists";
    let kanidm_name = "test-delete-person-when-idm-no-idm";
    let s = setup_kanidm_connection(kanidm_name).await;

    let person_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
        },
        "personAttributes": {
            "displayname": "Test Delete Person",
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;
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

    let opts = ListParams::default().fields(&format!(
        "type=Warning,involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    assert!(
        event_list
            .items
            .iter()
            .any(|e| e.reason == Some("KanidmClientError".to_string()))
    );
}

#[tokio::test]
async fn person_update_credential_token() {
    let name = "test-update-credential-token";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Test Update Credential",
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let person_uid = person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={person_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let token_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("TokenCreated".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(token_events.len(), 1);
    assert!(
        token_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains(&format!("https://{KANIDM_NAME}.localhost/ui/reset?token="))
    );

    // Delete, repeat and check that TokenCreated event is recreated
    person_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        person_api.clone(),
        name,
        conditions::is_deleted(&person_uid),
    )
    .await;

    let person_uid = person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={person_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let token_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("TokenCreated".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(token_events.len(), 1);
    assert!(
        token_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains(&format!("https://{KANIDM_NAME}.localhost/ui/reset?token="))
    );

    // Test with custom credentials_token_ttl (2 hours = 7200 seconds)
    let custom_ttl_name = "test-custom-credential-ttl";

    let custom_ttl_person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Test Custom TTL Credential",
        },
        "credentialsTokenTtl": 7200,
    });
    let custom_ttl_person = KanidmPersonAccount::new(
        custom_ttl_name,
        serde_json::from_value(custom_ttl_person_spec).unwrap(),
    );
    let custom_ttl_person_uid = person_api
        .create(&PostParams::default(), &custom_ttl_person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), custom_ttl_name, is_person("Exists")).await;
    wait_for(person_api.clone(), custom_ttl_name, is_person("Updated")).await;
    wait_for(person_api.clone(), custom_ttl_name, is_person_ready()).await;

    let custom_ttl_opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={custom_ttl_person_uid}"
    ));
    check_event_with_timeout(&event_api, &custom_ttl_opts).await;
    let custom_ttl_event_list = event_api.list(&custom_ttl_opts).await.unwrap();
    assert!(custom_ttl_event_list.items.is_empty().not());
    let custom_ttl_token_events = custom_ttl_event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("TokenCreated".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(custom_ttl_token_events.len(), 1);

    let event_message = custom_ttl_token_events
        .first()
        .unwrap()
        .message
        .as_deref()
        .unwrap();

    assert!(event_message.contains(&format!("https://{KANIDM_NAME}.localhost/ui/reset?token=")));
    let expire_part = event_message
        .split("This token will expire at: ")
        .nth(1)
        .unwrap();
    let expire_time_str = expire_part.trim();
    let expire_time = time::OffsetDateTime::parse(
        expire_time_str,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();

    let now = chrono::Utc::now();
    let expected_expire_time = now + chrono::Duration::seconds(7200);

    let expire_time_chrono =
        chrono::DateTime::from_timestamp(expire_time.unix_timestamp(), 0).unwrap();
    let time_diff = (expire_time_chrono - expected_expire_time)
        .num_seconds()
        .abs();

    assert!(
        time_diff <= 120,
        "Expected expiry time difference should be <= 120 seconds, got: {} seconds. Expected: {}, Got: {}",
        time_diff,
        expected_expire_time,
        expire_time_chrono
    );
}

#[tokio::test]
async fn person_attributes_collision() {
    let name = "test-person-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Attributes Collision",
            "mail": ["collision@example.com"],
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let collide_name = "test-person-attr-collide";
    let collide_person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Collide Person",
            "mail": ["collision@example.com"],
        },
    });
    let person = KanidmPersonAccount::new(
        collide_name,
        serde_json::from_value(collide_person_spec).unwrap(),
    );
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let person_uid = person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), collide_name, is_person("Exists")).await;
    wait_for(person_api.clone(), collide_name, is_person_false("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={person_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let token_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("KanidmError".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(token_events.len(), 1);
    assert!(
        token_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains("duplicate value detected")
    );
}

#[tokio::test]
async fn person_posix_attributes_collision() {
    let name = "test-person-posix-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Attributes Collision",
        },
        "posixAttributes": {
            "gidnumber": 1000,
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap();

    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person("Updated")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    let collide_name = "test-person-posix-attr-collide";
    let collide_person_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "personAttributes": {
            "displayname": "Collide Person",
        },
        "posixAttributes": {
            "gidnumber": 1000,
        },
    });
    let person = KanidmPersonAccount::new(
        collide_name,
        serde_json::from_value(collide_person_spec).unwrap(),
    );
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
    let person_uid = person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(person_api.clone(), collide_name, is_person("Exists")).await;
    wait_for(person_api.clone(), collide_name, is_person("Updated")).await;
    wait_for(
        person_api.clone(),
        collide_name,
        is_person_false("PosixUpdated"),
    )
    .await;
    wait_for(person_api.clone(), collide_name, is_person_ready().not()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={person_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let token_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("KanidmError".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(token_events.len(), 1);
    assert!(
        token_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains("duplicate value detected")
    );
}

#[tokio::test]
async fn person_different_namespace() {
    let name = "test-different-namespace";
    let kanidm_name = "test-different-namespace-kanidm";
    let s = setup_kanidm_connection(kanidm_name).await;
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(kanidm_name).await.unwrap();

    let person_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
            "namespace": "default",
        },
        "personAttributes": {
            "displayname": "Test Different Namespace",
            "mail": ["test@example.com"],
        },
    });
    let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
    let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "kaniop");
    let person_uid = person_api
        .create(&PostParams::default(), &person)
        .await
        .unwrap()
        .uid()
        .unwrap();

    let opts = ListParams::default().fields(&format!(
            "involvedObject.kind=KanidmPersonAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={person_uid}"
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
    assert!(
        token_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains(
                "configure `personNamespaceSelector` on Kanidm resource to watch this namespace"
            )
    );

    kanidm.metadata =
        serde_json::from_value(json!({"name": kanidm_name, "namespace": "default"})).unwrap();
    kanidm.spec.person_namespace_selector = serde_json::from_value(json!({})).unwrap();
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();
    person_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();
    wait_for(person_api.clone(), name, is_person("Exists")).await;
    wait_for(person_api.clone(), name, is_person_ready()).await;

    person_api.delete(name, &Default::default()).await.unwrap();
    wait_for(
        person_api.clone(),
        name,
        conditions::is_deleted(&person_uid),
    )
    .await;

    kanidm.spec.person_namespace_selector = serde_json::from_value(json!({
        "matchLabels": {
            "watch": "true"
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
}
