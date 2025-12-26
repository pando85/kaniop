use super::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_operator::crd::KanidmAccountPosixAttributes;
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_service_account::crd::KanidmServiceAccount;

use std::collections::BTreeSet;
use std::ops::Not;

use chrono::Utc;
use k8s_openapi::api::core::v1::{Event, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::api::DeleteParams;
use kube::{
    Api,
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::{conditions, wait::Condition},
};
use kube::{Client, ResourceExt};
use serde_json::json;

const KANIDM_NAME: &str = "test-service-account";

fn check_service_account_condition(
    cond: &str,
    status: String,
) -> impl Condition<KanidmServiceAccount> + '_ {
    move |obj: Option<&KanidmServiceAccount>| {
        obj.and_then(|sa| sa.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

fn is_service_account(cond: &str) -> impl Condition<KanidmServiceAccount> + '_ {
    check_service_account_condition(cond, "True".to_string())
}

fn is_service_account_false(cond: &str) -> impl Condition<KanidmServiceAccount> + '_ {
    check_service_account_condition(cond, "False".to_string())
}

fn is_service_account_ready() -> impl Condition<KanidmServiceAccount> {
    move |obj: Option<&KanidmServiceAccount>| {
        obj.and_then(|sa| sa.status.as_ref())
            .is_some_and(|status| status.ready)
    }
}

#[tokio::test]
async fn service_account_lifecycle() {
    let name = "test-service-account-lifecycle";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Foo",
            "entryManagedBy": "idm_admin",
            "mail": ["foo-sa@example.com"],
        },
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Valid")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let sa_created = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        sa_created
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Test SA Foo"
    );
    assert_eq!(
        sa_created.unwrap().attrs.get("mail").unwrap(),
        &["foo-sa@example.com".to_string()]
    );

    // Update the service account
    service_account.spec.service_account_attributes.displayname = "Test SA Bob".to_string();

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account_false("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let updated_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        updated_sa
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Test SA Bob"
    );
    assert!(
        updated_sa
            .clone()
            .unwrap()
            .attrs
            .contains_key("gidnumber")
            .not()
    );
    assert_eq!(
        updated_sa.unwrap().attrs.get("mail").unwrap(),
        &["foo-sa@example.com".to_string()]
    );

    // External modification of the service account - overwritten by the operator
    s.kanidm_client
        .idm_service_account_update(name, None, Some("changed_displayname"), None, None)
        .await
        .unwrap();
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account_false("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let external_updated_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        external_updated_sa
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Test SA Bob"
    );
    assert_eq!(
        external_updated_sa
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap(),
        &["foo-sa@example.com".to_string()]
    );

    // External modification of the service account - manually managed
    // we modify a non-managed attribute to verify operator doesn't overwrite it
    s.kanidm_client
        .idm_service_account_update(
            name,
            None,
            Some("Test SA Bob"), // keep displayname same
            None,
            Some(&[
                "foo-sa@example.com".to_string(),
                "bob-sa@example.com".to_string(),
            ]),
        )
        .await
        .unwrap();
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account_false("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let external_updated_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        external_updated_sa
            .clone()
            .unwrap()
            .attrs
            .get("displayname")
            .unwrap()
            .first()
            .unwrap(),
        "Test SA Bob"
    );
    // Operator overwrites mail attribute since it's in the spec
    assert_eq!(
        external_updated_sa
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap(),
        &["foo-sa@example.com".to_string()]
    );

    // Add Posix attributes
    service_account.spec.posix_attributes = Some(KanidmAccountPosixAttributes {
        ..Default::default()
    });

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();
    wait_for(sa_api.clone(), name, is_service_account("PosixUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let posix_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert!(
        posix_sa
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    service_account.spec.posix_attributes = Some(KanidmAccountPosixAttributes {
        loginshell: Some("/bin/bash".to_string()),
        ..Default::default()
    });

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();
    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("PosixUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("PosixUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let posix_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert!(
        posix_sa
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );
    assert_eq!(
        posix_sa
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
        .idm_service_account_unix_extend(name, None, Some("/usr/bin/nologin"))
        .await
        .unwrap();
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("PosixUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("PosixUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let external_posix_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        external_posix_sa
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
        external_posix_sa
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    // External modification of posix - manually managed
    // we modify the gidnumber attribute to know that the operator modified the object
    s.kanidm_client
        .idm_service_account_unix_extend(name, Some(555555), Some("/usr/bin/nologin"))
        .await
        .unwrap();
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("PosixUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("PosixUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let external_posix_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert_eq!(
        external_posix_sa
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
        external_posix_sa
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
    service_account.spec.posix_attributes = None;
    // we modify the displayname to know that the operator modified the object
    service_account.spec.service_account_attributes.displayname = "Test SA Foo".to_string();

    let posix_sa_uid = sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account_false("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
    let posix_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert!(
        posix_sa
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );
    assert_eq!(
        posix_sa
            .clone()
            .unwrap()
            .attrs
            .get("loginshell")
            .unwrap()
            .first()
            .unwrap(),
        "/bin/bash"
    );

    // Make the service account invalid
    service_account
        .spec
        .service_account_attributes
        .account_expire = Some(Time(Utc::now() - chrono::Duration::days(1)));

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account_false("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_false("Valid")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let invalid_sa = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert!(
        !invalid_sa
            .clone()
            .unwrap()
            .attrs
            .get("account_expire")
            .unwrap()
            .is_empty()
    );

    // Delete the service account
    sa_api.delete(name, &DeleteParams::default()).await.unwrap();
    wait_for(sa_api.clone(), name, conditions::is_deleted(&posix_sa_uid)).await;

    let result = s.kanidm_client.idm_service_account_get(name).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn service_account_create_no_idm() {
    let name = "test-sa-create-no-idm";
    let client = Client::try_default().await.unwrap();
    let sa_spec = json!({
        "kanidmRef": {
            "name": name,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Create",
            "entryManagedBy": "idm_admin",
        },
    });
    let service_account = KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmServiceAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
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

    let sa_result = sa_api.get(name).await.unwrap();
    assert!(sa_result.status.is_none());
}

#[tokio::test]
async fn service_account_delete_when_idm_no_longer_exists() {
    let name = "test-delete-sa-when-idm-no-longer-exists";
    let kanidm_name = "test-delete-sa-when-idm-no-idm";
    let s = setup_kanidm_connection(kanidm_name).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
        },
        "serviceAccountAttributes": {
            "displayname": "Test Delete SA",
            "entryManagedBy": "idm_admin",
        },
    });
    let service_account = KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
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

    sa_api.delete(name, &DeleteParams::default()).await.unwrap();

    let opts = ListParams::default().fields(&format!(
        "type=Warning,involvedObject.kind=KanidmServiceAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
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
async fn service_account_attributes_collision() {
    let name = "test-sa-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Attributes Collision",
            "entryManagedBy": "idm_admin",
            "mail": ["collision-sa@example.com"],
        },
    });
    let service_account = KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let collide_name = "test-sa-attr-collide";
    let collide_sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Collide SA",
            "entryManagedBy": "idm_admin",
            "mail": ["collision-sa@example.com"],
        },
    });
    let service_account = KanidmServiceAccount::new(
        collide_name,
        serde_json::from_value(collide_sa_spec).unwrap(),
    );
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    let sa_uid = sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(sa_api.clone(), collide_name, is_service_account("Exists")).await;
    wait_for(
        sa_api.clone(),
        collide_name,
        is_service_account_false("Updated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmServiceAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={sa_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let error_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("KanidmError".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(error_events.len(), 1);
    assert!(
        error_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains("AttributeUniqueness")
    );
}

#[tokio::test]
async fn service_account_posix_attributes_collision() {
    let name = "test-sa-posix-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Attributes Collision",
            "entryManagedBy": "idm_admin",
        },
        "posixAttributes": {
            "gidnumber": 1000,
        },
    });
    let service_account = KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let collide_name = "test-sa-posix-attr-collide";
    let collide_sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Collide SA",
            "entryManagedBy": "idm_admin",
        },
        "posixAttributes": {
            "gidnumber": 1000,
        },
    });
    let service_account = KanidmServiceAccount::new(
        collide_name,
        serde_json::from_value(collide_sa_spec).unwrap(),
    );
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    let sa_uid = sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(sa_api.clone(), collide_name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), collide_name, is_service_account("Updated")).await;
    wait_for(
        sa_api.clone(),
        collide_name,
        is_service_account_false("PosixUpdated"),
    )
    .await;
    wait_for(
        sa_api.clone(),
        collide_name,
        is_service_account_ready().not(),
    )
    .await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmServiceAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={sa_uid}"
    ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let error_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("KanidmError".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(error_events.len(), 1);
    assert!(
        error_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains("AttributeUniqueness")
    );
}

#[tokio::test]
async fn service_account_different_namespace() {
    let name = "test-different-namespace-sa";
    let kanidm_name = "test-different-namespace-kanidm-sa";
    let s = setup_kanidm_connection(kanidm_name).await;
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(kanidm_name).await.unwrap();

    let sa_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
            "namespace": "default",
        },
        "serviceAccountAttributes": {
            "displayname": "Test Different Namespace SA",
            "entryManagedBy": "idm_admin",
            "mail": [format!("{name}@example.com")],
        },
    });
    let service_account = KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "kaniop");
    let sa_uid = sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap()
        .uid()
        .unwrap();

    let opts = ListParams::default().fields(&format!(
            "involvedObject.kind=KanidmServiceAccount,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={sa_uid}"
        ));
    let event_api = Api::<Event>::namespaced(s.client.clone(), "kaniop");
    check_event_with_timeout(&event_api, &opts).await;
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(event_list.items.is_empty().not());
    let warning_events = event_list
        .items
        .iter()
        .filter(|e| e.reason == Some("ResourceNotWatched".to_string()))
        .collect::<Vec<_>>();
    assert_eq!(warning_events.len(), 1);
    assert!(
        warning_events
            .first()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains(
                "configure `serviceAccountNamespaceSelector` on Kanidm resource to watch this namespace"
            )
    );

    kanidm.metadata =
        serde_json::from_value(json!({"name": kanidm_name, "namespace": "default"})).unwrap();
    kanidm.spec.service_account_namespace_selector = serde_json::from_value(json!({})).unwrap();
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();
    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    sa_api.delete(name, &Default::default()).await.unwrap();
    wait_for(sa_api.clone(), name, conditions::is_deleted(&sa_uid)).await;

    kanidm.spec.service_account_namespace_selector = serde_json::from_value(json!({
        "matchLabels": {
            "watch-service-account": "true"
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
                "watch-service-account": "true"
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

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    kanidm.spec.service_account_namespace_selector = None;
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn service_account_api_tokens_lifecycle() {
    let name = "test-sa-api-tokens-lifecycle";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // 1. Initial Creation with API Tokens
    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA with API Tokens",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "readonly-token",
                "purpose": "readonly",
            },
            {
                "label": "readwrite-token",
                "purpose": "readwrite",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(30)).0.to_rfc3339(),
            },
        ],
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Verify tokens in Kanidm
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().any(|t| t.label == "readonly-token"));
    assert!(tokens.iter().any(|t| t.label == "readwrite-token"));

    // Verify secrets exist
    let readonly_secret_name = format!("{}-readonly-token-api-token", name);
    let readonly_secret = secret_api.get(&readonly_secret_name).await.unwrap();
    assert!(readonly_secret.data.is_some() || readonly_secret.string_data.is_some());
    assert_eq!(
        readonly_secret
            .metadata
            .labels
            .as_ref()
            .unwrap()
            .get("apitoken.kaniop.rs/label")
            .unwrap(),
        "readonly-token"
    );

    let rw_secret = secret_api.get("custom-rw-secret").await.unwrap();
    assert!(rw_secret.data.is_some() || rw_secret.string_data.is_some());
    assert_eq!(
        rw_secret
            .metadata
            .labels
            .as_ref()
            .unwrap()
            .get("apitoken.kaniop.rs/label")
            .unwrap(),
        "readwrite-token"
    );

    // Verify status
    let sa_status = sa_api.get(name).await.unwrap();
    assert_eq!(sa_status.status.as_ref().unwrap().api_tokens.len(), 2);
    let readonly_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readonly-token")
        .unwrap()
        .token_id
        .clone();
    let rw_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readwrite-token")
        .unwrap()
        .token_id
        .clone();

    // 2. Add New Token with Auto-generated Secret Name
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readwrite",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(30)).0.to_rfc3339(),
            }),
            json!({
                "label": "monitoring-token",
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 3);

    let monitoring_secret_name = format!("{}-monitoring-token-api-token", name);
    let monitoring_secret = secret_api.get(&monitoring_secret_name).await.unwrap();
    assert!(monitoring_secret.data.is_some() || monitoring_secret.string_data.is_some());

    let sa_status = sa_api.get(name).await.unwrap();
    assert_eq!(sa_status.status.as_ref().unwrap().api_tokens.len(), 3);

    // 3. Update Token Attributes (Purpose and Expiry)
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readonly",  // Changed from readwrite
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(60)).0.to_rfc3339(),  // Changed expiry
            }),
            json!({
                "label": "monitoring-token",
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Verify token was rotated (new token_id)
    let sa_status = sa_api.get(name).await.unwrap();
    let new_rw_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readwrite-token")
        .unwrap()
        .token_id
        .clone();
    assert_ne!(rw_token_id, new_rw_token_id, "Token should be rotated");

    // Secret should still exist
    secret_api.get("custom-rw-secret").await.unwrap();

    // 4. Secret Name Change (Token Rotation)
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
                "secretName": "my-custom-secret",  // Changed from None
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readonly",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(60)).0.to_rfc3339(),
            }),
            json!({
                "label": "monitoring-token",
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Old secret should be deleted
    assert!(secret_api.get(&readonly_secret_name).await.is_err());

    // New secret should exist
    secret_api.get("my-custom-secret").await.unwrap();

    // Token should be rotated
    let sa_status = sa_api.get(name).await.unwrap();
    let new_readonly_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readonly-token")
        .unwrap()
        .token_id
        .clone();
    assert_ne!(
        readonly_token_id, new_readonly_token_id,
        "Token should be rotated"
    );

    // 5. Change Secret Name to Another Custom Name
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
                "secretName": "another-secret",  // Changed again
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readonly",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(60)).0.to_rfc3339(),
            }),
            json!({
                "label": "monitoring-token",
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Old secret should be deleted
    assert!(secret_api.get("my-custom-secret").await.is_err());

    // New secret should exist
    secret_api.get("another-secret").await.unwrap();

    // Token should be rotated again
    let sa_status = sa_api.get(name).await.unwrap();
    let newest_readonly_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readonly-token")
        .unwrap()
        .token_id
        .clone();
    assert_ne!(
        new_readonly_token_id, newest_readonly_token_id,
        "Token should be rotated again"
    );

    // 6. Change Token Label (Token Rotation)
    let monitoring_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "monitoring-token")
        .unwrap()
        .token_id
        .clone();

    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
                "secretName": "another-secret",
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readonly",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() + chrono::Duration::days(60)).0.to_rfc3339(),
            }),
            json!({
                "label": "observability-token",  // Changed label
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Old token should be deleted
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert!(!tokens.iter().any(|t| t.label == "monitoring-token"));
    assert!(tokens.iter().any(|t| t.label == "observability-token"));

    // Old secret should be deleted, new secret created
    assert!(secret_api.get(&monitoring_secret_name).await.is_err());
    let observability_secret_name = format!("{}-observability-token-api-token", name);
    secret_api.get(&observability_secret_name).await.unwrap();

    // New token should have different token_id
    let sa_status = sa_api.get(name).await.unwrap();
    let observability_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "observability-token")
        .unwrap()
        .token_id
        .clone();
    assert_ne!(monitoring_token_id, observability_token_id);

    // 7. Expired Token
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "readonly-token",
                "purpose": "readonly",
                "secretName": "another-secret",
            }),
            json!({
                "label": "readwrite-token",
                "purpose": "readonly",
                "secretName": "custom-rw-secret",
                "expiry": Time(Utc::now() - chrono::Duration::days(1)).0.to_rfc3339(),  // Expired
            }),
            json!({
                "label": "observability-token",
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Token should still exist in Kanidm
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert!(tokens.iter().any(|t| t.label == "readwrite-token"));

    // Secret should still exist
    secret_api.get("custom-rw-secret").await.unwrap();

    // Status should reflect expired time
    let sa_status = sa_api.get(name).await.unwrap();
    let expired_token = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "readwrite-token")
        .unwrap();
    assert!(expired_token.expiry.is_some());

    // 8. Remove Tokens
    service_account.spec.api_tokens = Some(
        [json!({
            "label": "readonly-token",
            "purpose": "readonly",
            "secretName": "another-secret",
        })]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Tokens should be deleted from Kanidm
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(!tokens.iter().any(|t| t.label == "readwrite-token"));
    assert!(!tokens.iter().any(|t| t.label == "observability-token"));

    // Secrets should be deleted
    assert!(secret_api.get("custom-rw-secret").await.is_err());
    assert!(secret_api.get(&observability_secret_name).await.is_err());

    // Status should show only remaining token
    let sa_status = sa_api.get(name).await.unwrap();
    assert_eq!(sa_status.status.as_ref().unwrap().api_tokens.len(), 1);

    // 9. External Token Deletion (Operator Recreation)
    let remaining_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .first()
        .unwrap()
        .token_id
        .clone();

    // Manually delete token from Kanidm
    s.kanidm_client
        .idm_service_account_destroy_api_token(
            name,
            uuid::Uuid::parse_str(&remaining_token_id).unwrap(),
        )
        .await
        .unwrap();

    // Trigger reconciliation
    sa_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Token should be recreated
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(tokens.iter().any(|t| t.label == "readonly-token"));

    // Secret should be updated with new token value
    secret_api.get("another-secret").await.unwrap();

    // Status should reflect new token_id
    let sa_status = sa_api.get(name).await.unwrap();
    let recreated_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .first()
        .unwrap()
        .token_id
        .clone();
    assert_ne!(
        remaining_token_id, recreated_token_id,
        "Token should be recreated with new ID"
    );
}

#[tokio::test]
async fn service_account_api_token_invalid_secret_name() {
    let name = "test-sa-invalid-secret";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create SA with invalid secret names
    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Invalid Secret Name",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "invalid-uppercase",
                "purpose": "readonly",
                "secretName": "InvalidName",  // Uppercase not allowed
            },
        ],
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(result.is_err(), "Should fail validation");
    let error_message = format!("{:?}", result.unwrap_err());
    assert!(error_message.contains("secretName") || error_message.contains("InvalidName"));

    // Test with underscore
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "invalid-underscore",
        "purpose": "readonly",
        "secretName": "invalid_name",
    }))
    .unwrap()]));

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(result.is_err(), "Should fail validation");

    // Test with leading hyphen
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "invalid-leading-hyphen",
        "purpose": "readonly",
        "secretName": "-invalid",
    }))
    .unwrap()]));

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(result.is_err(), "Should fail validation");

    // Test with trailing hyphen
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "invalid-trailing-hyphen",
        "purpose": "readonly",
        "secretName": "invalid-",
    }))
    .unwrap()]));

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(result.is_err(), "Should fail validation");

    // Test with special characters
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "invalid-special-chars",
        "purpose": "readonly",
        "secretName": "inv@lid!",
    }))
    .unwrap()]));

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(result.is_err(), "Should fail validation");

    // Test valid names work: lowercase with hyphens
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "valid-token",
        "purpose": "readonly",
        "secretName": "valid-secret-name",
    }))
    .unwrap()]));

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Test valid name with dots
    service_account.spec.api_tokens = Some(BTreeSet::from([serde_json::from_value(json!({
        "label": "valid-token-with-dots",
        "purpose": "readonly",
        "secretName": "valid.secret.name",
    }))
    .unwrap()]));

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;
}

#[tokio::test]
async fn service_account_api_token_duplicate_labels() {
    let name = "test-sa-duplicate-labels";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Duplicate Labels",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "my-token",
                "purpose": "readonly",
            },
        ],
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();
    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Attempt to add duplicate label
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "my-token",
                "purpose": "readonly",
            }),
            json!({
                "label": "my-token",  // Duplicate
                "purpose": "readwrite",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    let result = sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await;

    let error_message = format!("{:?}", result.unwrap_err());
    assert!(
        error_message.contains("label must be unique across all API tokens (case-insensitive).")
    );

    // Fix by renaming the second token
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "my-token",
                "purpose": "readonly",
            }),
            json!({
                "label": "another-token",  // Fixed
                "purpose": "readwrite",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Verify both tokens exist
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().any(|t| t.label == "my-token"));
    assert!(tokens.iter().any(|t| t.label == "another-token"));

    // Now test changing one token's label to duplicate another (via update)
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "my-token",
                "purpose": "readonly",
            }),
            json!({
                "label": "my-token",  // Changed to duplicate
                "purpose": "readwrite",
                "secretName": "custom-secret",  // Different secret to make it "different"
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    let result = sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await;

    let error_message = format!("{:?}", result.unwrap_err());
    assert!(
        error_message.contains("label must be unique across all API tokens (case-insensitive).")
    );
}

#[tokio::test]
async fn service_account_api_token_duplicate_secret_names() {
    let name = "test-sa-duplicate-secret-names";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create SA with duplicate secret names
    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Duplicate Secret Names",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "token-one",
                "purpose": "readonly",
                "secretName": "shared-secret",  // Same secret name
            },
            {
                "label": "token-two",
                "purpose": "readwrite",
                "secretName": "shared-secret",  // Duplicate
            },
        ],
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");

    // Should fail validation at API level
    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(
        result.is_err(),
        "Should fail validation for duplicate secret names"
    );
    let error_message = format!("{:?}", result.unwrap_err());
    assert!(
        error_message
            .contains("secretName must be unique across all API tokens (case-insensitive).")
    );

    // Test case-insensitive duplicate detection
    let sa_spec_case = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Duplicate Secret Names Case",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "token-alpha",
                "purpose": "readonly",
                "secretName": "my-secret",
            },
            {
                "label": "token-beta",
                "purpose": "readwrite",
                "secretName": "My-Secret",  // Same name, different case
            },
        ],
    });
    service_account = KanidmServiceAccount::new(
        &format!("{}-case", name),
        serde_json::from_value(sa_spec_case).unwrap(),
    );

    let result = sa_api
        .create(&PostParams::default(), &service_account)
        .await;
    assert!(
        result.is_err(),
        "Should fail validation for case-insensitive duplicate"
    );

    // Valid case: different secret names
    let sa_spec_valid = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Valid Secret Names",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "token-one",
                "purpose": "readonly",
                "secretName": "secret-one",
            },
            {
                "label": "token-two",
                "purpose": "readwrite",
                "secretName": "secret-two",
            },
        ],
    });
    service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec_valid).unwrap());

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Verify both tokens were created
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);

    // Now test changing one token's secret name to duplicate another (via update)
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "token-one",
                "purpose": "readonly",
                "secretName": "secret-one",
            }),
            json!({
                "label": "token-two",
                "purpose": "readwrite",
                "secretName": "secret-one",  // Changed to duplicate
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    let result = sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await;

    let error_message = format!("{:?}", result.unwrap_err());
    assert!(
        error_message
            .contains("secretName must be unique across all API tokens (case-insensitive).")
    );

    // Valid case: one with secretName, one without (auto-generated)
    service_account.spec.api_tokens = Some(
        [
            json!({
                "label": "token-one",
                "purpose": "readonly",
                "secretName": "secret-one",
            }),
            json!({
                "label": "token-three",  // New token with auto-generated name
                "purpose": "readonly",
            }),
        ]
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect(),
    );

    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("ApiTokensUpdated"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Verify tokens exist
    let tokens = s
        .kanidm_client
        .idm_service_account_list_api_token(name)
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().any(|t| t.label == "token-one"));
    assert!(tokens.iter().any(|t| t.label == "token-three"));
}

#[tokio::test]
async fn service_account_credentials() {
    let name = "test-service-account-credentials";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Foo",
            "entryManagedBy": "idm_admin",
        },
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("Updated")).await;
    wait_for(sa_api.clone(), name, is_service_account("Valid")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    let credentials_err = s
        .kanidm_client
        .idm_service_account_get_credential_status(name)
        .await
        .unwrap_err();
    let error_message = format!("{:?}", credentials_err);
    assert_eq!(error_message, "EmptyResponse");

    service_account.spec.generate_credentials = true;
    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();
    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("CredentialsInitialized"),
    )
    .await;
    wait_for(
        sa_api.clone(),
        name,
        is_service_account("CredentialsInitialized"),
    )
    .await;

    let credential_status = s
        .kanidm_client
        .idm_service_account_get_credential_status(name)
        .await
        .unwrap();
    assert_eq!(credential_status.creds.len(), 1);

    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let credentials_secret_name = format!("{}-kanidm-service-account-credentials", name);
    let credentials_secret = secret_api.get(&credentials_secret_name).await.unwrap();
    let credentials_data = credentials_secret.data.unwrap();
    let password = credentials_data.get("password").unwrap();

    secret_api
        .delete(&credentials_secret_name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        sa_api.clone(),
        name,
        is_service_account_false("CredentialsInitialized"),
    )
    .await;
    wait_for(
        sa_api.clone(),
        name,
        is_service_account("CredentialsInitialized"),
    )
    .await;

    let new_credentials_secret = secret_api.get(&credentials_secret_name).await.unwrap();
    let new_credentials_data = new_credentials_secret.data.unwrap();
    let new_password = new_credentials_data.get("password").unwrap();
    assert_ne!(password, new_password, "Password should be rotated");

    service_account.spec.generate_credentials = false;
    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    let secret = secret_api.get(&credentials_secret_name).await.unwrap();
    wait_for(
        secret_api.clone(),
        &secret.name_any(),
        conditions::is_deleted(&secret.uid().unwrap()),
    )
    .await;
}

#[tokio::test]
async fn service_account_duplicate_across_namespaces() {
    let name = "test-sa-duplicate-across-namespaces";
    let kanidm_name = "test-duplicate-ns-kanidm-sa";
    let s = setup_kanidm_connection(kanidm_name).await;
    let kanidm_api = Api::<Kanidm>::namespaced(s.client.clone(), "default");
    let mut kanidm = kanidm_api.get(kanidm_name).await.unwrap();

    // Configure namespace selector to watch all namespaces
    kanidm.metadata =
        serde_json::from_value(json!({"name": kanidm_name, "namespace": "default"})).unwrap();
    kanidm.spec.service_account_namespace_selector = serde_json::from_value(json!({})).unwrap();
    kanidm_api
        .patch(
            kanidm_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&kanidm),
        )
        .await
        .unwrap();

    // Create first service account in default namespace
    let sa_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
            "namespace": "default",
        },
        "serviceAccountAttributes": {
            "displayname": "Test Service Account",
            "entryManagedBy": "idm_admin",
        },
    });
    let service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec.clone()).unwrap());
    let sa_api_default = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    sa_api_default
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api_default.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api_default.clone(), name, is_service_account_ready()).await;

    // Try to create second service account with same name in kaniop namespace
    // Should be rejected by admission webhook
    let duplicate_sa =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec.clone()).unwrap());
    let sa_api_kaniop = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "kaniop");
    let result = sa_api_kaniop
        .create(&PostParams::default(), &duplicate_sa)
        .await;

    // Verify the duplicate creation was rejected
    assert!(result.is_err());
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.contains("already exists") || error_message.contains("duplicate"),
        "Expected duplicate error, got: {}",
        error_message
    );
}
#[tokio::test]
async fn service_account_credentials_rotation() {
    let name = "test-service-account-creds-rotation";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create service account with credentials rotation enabled (1 day period for testing)
    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Rotation",
            "entryManagedBy": "idm_admin",
        },
        "generateCredentials": true,
        "credentialsRotation": {
            "enabled": true,
            "periodDays": 1,
        },
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(
        sa_api.clone(),
        name,
        is_service_account("CredentialsInitialized"),
    )
    .await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Get the initial secret and verify rotation annotations
    let credentials_secret_name = format!("{}-kanidm-service-account-credentials", name);
    let initial_secret = secret_api.get(&credentials_secret_name).await.unwrap();

    // Verify rotation annotations exist
    let annotations = initial_secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"true".to_string()),
        "Rotation should be enabled in annotations"
    );
    assert_eq!(
        annotations.get("kaniop.rs/rotation-period-days"),
        Some(&"1".to_string()),
        "Rotation period should be 1 day"
    );
    assert!(
        annotations.contains_key("kaniop.rs/last-rotation-time"),
        "Last rotation time should be set"
    );

    let initial_password = initial_secret
        .data
        .as_ref()
        .unwrap()
        .get("password")
        .unwrap()
        .clone();

    // Simulate time passing by manually setting last-rotation-time to 2 days ago
    let two_days_ago = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    let mut secret_patch = initial_secret.clone();
    // Clear managed_fields to avoid "metadata.managedFields must be nil" error
    secret_patch.metadata.managed_fields = None;
    secret_patch.metadata.annotations.as_mut().unwrap().insert(
        "kaniop.rs/last-rotation-time".to_string(),
        two_days_ago.clone(),
    );

    secret_api
        .patch(
            &credentials_secret_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&secret_patch),
        )
        .await
        .unwrap();

    // Trigger reconciliation by updating the service account
    service_account.metadata.annotations = Some(
        [(
            "trigger-reconciliation".to_string(),
            "rotation-test".to_string(),
        )]
        .iter()
        .cloned()
        .collect(),
    );
    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    // Wait for rotation to occur (CredentialsInitialized goes False then True)
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Get the rotated secret
    let rotated_secret = secret_api.get(&credentials_secret_name).await.unwrap();

    let rotated_password = rotated_secret
        .data
        .as_ref()
        .unwrap()
        .get("password")
        .unwrap()
        .clone();

    // Verify password was rotated
    assert_ne!(
        initial_password, rotated_password,
        "Password should have been rotated"
    );

    // Verify rotation time was updated
    let rotated_annotations = rotated_secret.metadata.annotations.as_ref().unwrap();
    let new_rotation_time = rotated_annotations
        .get("kaniop.rs/last-rotation-time")
        .unwrap();
    assert_ne!(
        new_rotation_time, &two_days_ago,
        "Rotation timestamp should have been updated"
    );

    // Verify the new timestamp is recent (within last minute)
    let new_time = new_rotation_time.parse::<chrono::DateTime<Utc>>().unwrap();
    let now = Utc::now();
    let diff = now.signed_duration_since(new_time);
    assert!(
        diff.num_seconds() < 60,
        "New rotation time should be within the last minute"
    );
}

#[tokio::test]
async fn service_account_api_token_rotation() {
    let name = "test-service-account-token-rotation";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create service account with API token rotation enabled (1 day period for testing)
    let sa_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "serviceAccountAttributes": {
            "displayname": "Test SA Token Rotation",
            "entryManagedBy": "idm_admin",
        },
        "apiTokens": [
            {
                "label": "rotation-test-token",
                "purpose": "readonly",
            }
        ],
        "apiTokenRotation": {
            "enabled": true,
            "periodDays": 1,
        },
    });
    let mut service_account =
        KanidmServiceAccount::new(name, serde_json::from_value(sa_spec).unwrap());
    let sa_api = Api::<KanidmServiceAccount>::namespaced(s.client.clone(), "default");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");

    sa_api
        .create(&PostParams::default(), &service_account)
        .await
        .unwrap();

    wait_for(sa_api.clone(), name, is_service_account("Exists")).await;
    wait_for(sa_api.clone(), name, is_service_account("ApiTokensUpdated")).await;
    wait_for(sa_api.clone(), name, is_service_account_ready()).await;

    // Get the initial token secret and verify rotation annotations
    let token_secret_name = format!("{}-rotation-test-token-api-token", name);
    let initial_secret = secret_api.get(&token_secret_name).await.unwrap();

    // Verify rotation annotations exist
    let annotations = initial_secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"true".to_string()),
        "Rotation should be enabled in annotations"
    );
    assert_eq!(
        annotations.get("kaniop.rs/rotation-period-days"),
        Some(&"1".to_string()),
        "Rotation period should be 1 day"
    );

    let initial_token = initial_secret
        .data
        .as_ref()
        .unwrap()
        .get("token")
        .unwrap()
        .clone();

    // Get initial token ID from status
    let sa_status = sa_api.get(name).await.unwrap();
    let initial_token_id = sa_status
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "rotation-test-token")
        .unwrap()
        .token_id
        .clone();

    // Simulate time passing by manually setting last-rotation-time to 2 days ago
    let two_days_ago = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    let mut secret_patch = initial_secret.clone();
    // Clear managed_fields to avoid "metadata.managedFields must be nil" error
    secret_patch.metadata.managed_fields = None;
    secret_patch.metadata.annotations.as_mut().unwrap().insert(
        "kaniop.rs/last-rotation-time".to_string(),
        two_days_ago.clone(),
    );

    secret_api
        .patch(
            &token_secret_name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&secret_patch),
        )
        .await
        .unwrap();

    // Trigger reconciliation by updating the service account
    service_account.metadata.annotations = Some(
        [(
            "trigger-reconciliation".to_string(),
            "token-rotation-test".to_string(),
        )]
        .iter()
        .cloned()
        .collect(),
    );
    sa_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&service_account),
        )
        .await
        .unwrap();

    // Wait for rotation to occur
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Get the rotated secret
    let rotated_secret = secret_api.get(&token_secret_name).await.unwrap();

    let rotated_token = rotated_secret
        .data
        .as_ref()
        .unwrap()
        .get("token")
        .unwrap()
        .clone();

    // Verify token was rotated
    assert_ne!(
        initial_token, rotated_token,
        "Token should have been rotated"
    );

    // Verify token ID changed in Kanidm (token was destroyed and recreated)
    let sa_status_after = sa_api.get(name).await.unwrap();
    let rotated_token_id = sa_status_after
        .status
        .as_ref()
        .unwrap()
        .api_tokens
        .iter()
        .find(|t| t.label == "rotation-test-token")
        .unwrap()
        .token_id
        .clone();

    assert_ne!(
        initial_token_id, rotated_token_id,
        "Token ID should have changed (token was recreated)"
    );

    // Verify rotation time was updated
    let rotated_annotations = rotated_secret.metadata.annotations.as_ref().unwrap();
    let new_rotation_time = rotated_annotations
        .get("kaniop.rs/last-rotation-time")
        .unwrap();
    assert_ne!(
        new_rotation_time, &two_days_ago,
        "Rotation timestamp should have been updated"
    );

    // Verify the new timestamp is recent (within last minute)
    let new_time = new_rotation_time.parse::<chrono::DateTime<Utc>>().unwrap();
    let now = Utc::now();
    let diff = now.signed_duration_since(new_time);
    assert!(
        diff.num_seconds() < 60,
        "New rotation time should be within the last minute"
    );
}
