use super::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_group::crd::{KanidmGroup, KanidmGroupPosixAttributes};
use kaniop_operator::kanidm::crd::Kanidm;

use std::ops::Not;

use chrono::Utc;
use k8s_openapi::api::core::v1::Event;
use kube::api::DeleteParams;
use kube::{
    Api,
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::{conditions, wait::Condition},
};
use kube::{Client, ResourceExt};
use serde_json::json;

const KANIDM_NAME: &str = "test-group";

fn check_group_condition(cond: &str, status: String) -> impl Condition<KanidmGroup> + '_ {
    move |obj: Option<&KanidmGroup>| {
        obj.and_then(|group| group.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == cond && c.status == status)
            })
    }
}

fn is_group(cond: &str) -> impl Condition<KanidmGroup> + '_ {
    check_group_condition(cond, "True".to_string())
}

fn is_group_false(cond: &str) -> impl Condition<KanidmGroup> + '_ {
    check_group_condition(cond, "False".to_string())
}

fn is_group_ready() -> impl Condition<KanidmGroup> {
    move |obj: Option<&KanidmGroup>| {
        obj.and_then(|group| group.status.as_ref())
            .is_some_and(|status| status.ready)
    }
}

#[tokio::test]
async fn group_lifecycle() {
    let name = "test-group-lifecycle";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "mail": ["test-group-lifecycle@example.com"],
    });
    let mut group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group("Exists")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;

    let group_created = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert!(group_created.is_some());
    assert_eq!(
        group_created
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap()
            .first()
            .unwrap(),
        "test-group-lifecycle@example.com"
    );

    // Update the group
    group.spec.mail = Some(vec!["updated-email@example.com".to_string()]);

    group_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&group),
        )
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group_false("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let updated_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert_eq!(
        updated_group
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap()
            .first()
            .unwrap(),
        "updated-email@example.com"
    );
    assert!(
        updated_group
            .clone()
            .unwrap()
            .attrs
            .contains_key("gidnumber")
            .not()
    );

    // External modification of the group - overwritten by the operator
    s.kanidm_client
        .idm_group_set_mail(name, &["try-new-email@example.com".to_string()])
        .await
        .unwrap();
    group_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group_false("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let external_updated_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert_eq!(
        external_updated_group
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap()
            .first()
            .unwrap(),
        "updated-email@example.com"
    );

    // External modification of the group members - manually managed
    s.kanidm_client
        .idm_group_add_members(name, &["admin"])
        .await
        .unwrap();

    // ensure we wait for the changes to be applied
    s.kanidm_client
        .idm_group_set_mail(name, &["try-new-email@example.com".to_string()])
        .await
        .unwrap();
    group_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group_false("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let external_updated_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert_eq!(
        external_updated_group
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap()
            .first()
            .unwrap(),
        "updated-email@example.com"
    );
    assert_eq!(
        external_updated_group
            .clone()
            .unwrap()
            .attrs
            .get("member")
            .unwrap()
            .first()
            .unwrap(),
        "admin@test-group.localhost"
    );

    // External modification of the group members - overwritten by the operator
    group.spec.members = Some(vec!["idm_admin".to_string()]);

    group_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&group),
        )
        .await
        .unwrap();
    wait_for(group_api.clone(), name, is_group_false("MembersUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MembersUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let external_updated_group = s.kanidm_client.idm_group_get(name).await.unwrap();

    let entry = external_updated_group.clone().unwrap();
    let members = entry.attrs.get("member").unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members.first().unwrap(), "idm_admin@test-group.localhost");

    // Add Posix attributes
    group.spec.posix_attributes = Some(KanidmGroupPosixAttributes {
        ..Default::default()
    });

    group_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&group),
        )
        .await
        .unwrap();
    wait_for(group_api.clone(), name, is_group("PosixUpdated")).await;
    let posix_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert!(
        posix_group
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    // External modification of posix - manually managed
    s.kanidm_client
        .idm_group_unix_extend(name, Some(555555))
        .await
        .unwrap();
    // ensure we wait for the changes to be applied
    s.kanidm_client
        .idm_group_set_mail(name, &["try-new-email@example.com".to_string()])
        .await
        .unwrap();
    group_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Utc::now().to_rfc3339()}}})),
        )
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group_false("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let external_posix_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert_eq!(
        external_posix_group
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
        external_updated_group
            .clone()
            .unwrap()
            .attrs
            .get("mail")
            .unwrap()
            .first()
            .unwrap(),
        "updated-email@example.com"
    );

    // External modification of posix - overwritten by the operator
    group.spec.posix_attributes = Some(KanidmGroupPosixAttributes {
        gidnumber: Some(666666),
    });
    group_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&group),
        )
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group_false("PosixUpdated")).await;
    wait_for(group_api.clone(), name, is_group("PosixUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    wait_for(group_api.clone(), name, |obj: Option<&KanidmGroup>| {
        obj.and_then(|obj| obj.status.as_ref())
            .is_some_and(|s| s.gid == Some(666666))
    })
    .await;
    let external_posix_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert_eq!(
        external_posix_group
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .first()
            .unwrap(),
        "666666"
    );

    // Keep Posix attributes
    group.spec.posix_attributes = None;
    // ensure we wait for the changes to be applied
    s.kanidm_client
        .idm_group_set_mail(name, &["try-new-email@example.com".to_string()])
        .await
        .unwrap();

    let posix_group_uid = group_api
        .patch(
            name,
            &PatchParams::apply("e2e-test").force(),
            &Patch::Apply(&group),
        )
        .await
        .unwrap()
        .uid()
        .unwrap();
    wait_for(group_api.clone(), name, is_group_false("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;
    let posix_group = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert!(
        posix_group
            .clone()
            .unwrap()
            .attrs
            .get("gidnumber")
            .unwrap()
            .is_empty()
            .not()
    );

    // Delete the group
    group_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();
    wait_for(
        group_api.clone(),
        name,
        conditions::is_deleted(&posix_group_uid),
    )
    .await;

    let result = s.kanidm_client.idm_group_get(name).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn group_create_no_idm() {
    let name = "test-group-create-no-idm";
    let client = Client::try_default().await.unwrap();
    let group_spec = json!({
        "kanidmRef": {
            "name": name,
        },
    });
    let group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmGroup,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
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

    let group_result = group_api.get(name).await.unwrap();
    assert!(group_result.status.is_none());
}

#[tokio::test]
async fn group_delete_group_when_idm_no_longer_exists() {
    let name = "test-delete-group-when-idm-no-longer-exists";
    let kanidm_name = "test-delete-group-when-idm-no-idm";
    let s = setup_kanidm_connection(kanidm_name).await;

    let group_spec = json!({
        "kanidmRef": {
            "name": kanidm_name,
        },
    });
    let group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group("Exists")).await;
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

    group_api
        .delete(name, &DeleteParams::default())
        .await
        .unwrap();

    let opts = ListParams::default().fields(&format!(
        "type=Warning,involvedObject.kind=KanidmGroup,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.name={name}"
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
async fn group_attributes_collision() {
    let name = "test-group-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "mail": ["same@example.com"]
    });
    let group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group("Exists")).await;
    wait_for(group_api.clone(), name, is_group("MailUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;

    let collide_name = "test-group-attr-collide";
    let collide_group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "mail": ["same@example.com"]
    });
    let group = KanidmGroup::new(
        collide_name,
        serde_json::from_value(collide_group_spec).unwrap(),
    );
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    let group_uid = group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(group_api.clone(), collide_name, is_group("Exists")).await;
    wait_for(
        group_api.clone(),
        collide_name,
        is_group_false("MailUpdated"),
    )
    .await;
    wait_for(group_api.clone(), collide_name, is_group_ready().not()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmGroup,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={group_uid}"
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
async fn group_posix_attributes_collision() {
    let name = "test-group-posix-attributes-collision";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "posixAttributes": {
            "gidnumber": 111111,
        },
    });
    let group = KanidmGroup::new(name, serde_json::from_value(group_spec).unwrap());
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap();

    wait_for(group_api.clone(), name, is_group("Exists")).await;
    wait_for(group_api.clone(), name, is_group("PosixUpdated")).await;
    wait_for(group_api.clone(), name, is_group_ready()).await;

    let collide_name = "test-group-posix-attr-collide";
    let collide_group_spec = json!({
        "kanidmRef": {
            "name": KANIDM_NAME,
        },
        "posixAttributes": {
            "gidnumber": 111111,
        },
    });
    let group = KanidmGroup::new(
        collide_name,
        serde_json::from_value(collide_group_spec).unwrap(),
    );
    let group_api = Api::<KanidmGroup>::namespaced(s.client.clone(), "default");
    let group_uid = group_api
        .create(&PostParams::default(), &group)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(group_api.clone(), collide_name, is_group("Exists")).await;
    wait_for(
        group_api.clone(),
        collide_name,
        is_group_false("PosixUpdated"),
    )
    .await;
    wait_for(group_api.clone(), collide_name, is_group_ready().not()).await;

    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmGroup,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={group_uid}"
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
