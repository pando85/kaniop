use super::{check_event_with_timeout, setup_kanidm_connection, wait_for};

use kaniop_oauth2::crd::KanidmOAuth2Client;

use std::ops::Not;

use k8s_openapi::api::core::v1::{Event, Secret};
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api, ResourceExt,
    api::{ListParams, Patch, PatchParams, PostParams},
    runtime::wait::Condition,
};
use serde_json::json;

const KANIDM_NAME: &str = "test-oauth2";
const MAIN_MANAGER: &str = "kanidmoauth2clients.kaniop.rs";
const TEMPLATE_MANAGER: &str = "template.kanidmoauth2clients.kaniop.rs";

fn is_oauth2(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|o| o.status.as_ref())
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conds| conds.iter().any(|c| c.type_ == cond && c.status == "True"))
    }
}

fn is_oauth2_false(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|o| o.status.as_ref())
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conds| conds.iter().any(|c| c.type_ == cond && c.status == "False"))
    }
}

fn is_oauth2_ready() -> impl Condition<KanidmOAuth2Client> {
    move |obj: Option<&KanidmOAuth2Client>| {
        obj.and_then(|o| o.status.as_ref()).is_some_and(|s| s.ready)
    }
}

/// Returns true once status.ready is true AND the named condition is absent from the conditions list.
/// The ready check acts as a stabilization guard: it ensures the reconciler has committed its
/// latest status update before we conclude the condition is absent.
fn is_oauth2_condition_absent(cond: &str) -> impl Condition<KanidmOAuth2Client> + '_ {
    move |obj: Option<&KanidmOAuth2Client>| {
        let Some(oauth2) = obj else {
            return false;
        };
        let Some(status) = &oauth2.status else {
            return false;
        };
        if !status.ready {
            return false;
        }
        status
            .conditions
            .as_ref()
            .is_none_or(|conds| conds.iter().all(|c| c.type_ != cond))
    }
}

// Basic application of template
#[tokio::test]
async fn oauth2_secret_template_labels() {
    let name = "test-st-labels";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Labels Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/env": "test",
                "example.com/team": "platform"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/env"),
        Some(&"test".to_string()),
        "template label example.com/env should be set"
    );
    assert_eq!(
        labels.get("example.com/team"),
        Some(&"platform".to_string()),
        "template label example.com/team should be set"
    );

    // Operator-managed labels must still be present
    assert!(
        labels.contains_key("app.kubernetes.io/name"),
        "operator name label must not be lost"
    );
    assert!(
        labels.contains_key("app.kubernetes.io/instance"),
        "operator instance label must not be lost"
    );
    assert!(
        labels.contains_key("app.kubernetes.io/managed-by"),
        "operator managed-by label must not be lost"
    );

    // status.secretName must be populated
    let status = oauth2_api.get(name).await.unwrap().status.unwrap();
    assert_eq!(
        status.secret_name,
        Some(secret_name),
        "status.secretName should be populated"
    );
}

#[tokio::test]
async fn oauth2_secret_template_annotations() {
    let name = "test-st-annotations";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Annotations Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "annotations": {
                "example.com/owner": "platform-team",
                "example.com/environment": "testing"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("example.com/owner"),
        Some(&"platform-team".to_string()),
        "template annotation example.com/owner should be set"
    );
    assert_eq!(
        annotations.get("example.com/environment"),
        Some(&"testing".to_string()),
        "template annotation example.com/environment should be set"
    );
}

#[tokio::test]
async fn oauth2_secret_template_labels_and_annotations() {
    let name = "test-st-labels-annots";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Labels+Annotations Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/tier": "backend"
            },
            "annotations": {
                "example.com/contact": "ops@example.com"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/tier"),
        Some(&"backend".to_string()),
        "template label example.com/tier should be set"
    );

    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("example.com/contact"),
        Some(&"ops@example.com".to_string()),
        "template annotation example.com/contact should be set"
    );
}

// Lifecycle changes after creation
#[tokio::test]
async fn oauth2_secret_template_add_after_creation() {
    let name = "test-st-add-after";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create WITHOUT secretTemplate
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Add After Creation Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    // Assert SecretTemplateSynced is absent before the patch
    let initial = oauth2_api.get(name).await.unwrap();
    let initial_conditions = initial.status.as_ref().and_then(|s| s.conditions.as_ref());
    assert!(
        initial_conditions
            .is_none_or(|conds| conds.iter().all(|c| c.type_ != "SecretTemplateSynced")),
        "SecretTemplateSynced should be absent before secretTemplate is set"
    );

    // Patch to add secretTemplate.labels
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "secretTemplate": {
                        "labels": {
                            "example.com/added-later": "yes"
                        }
                    }
                }
            })),
        )
        .await
        .unwrap();

    // Condition should transition False → True
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/added-later"),
        Some(&"yes".to_string()),
        "label added via patch should appear on Secret"
    );
}

#[tokio::test]
async fn oauth2_secret_template_remove() {
    let name = "test-st-remove";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Remove Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/to-be-removed": "value"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Remove secretTemplate entirely
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({"spec": {"secretTemplate": null}})),
        )
        .await
        .unwrap();

    // Wait for SecretTemplateSynced to become absent (via False → absent)
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_condition_absent("SecretTemplateSynced"),
    )
    .await;

    // The previously applied labels must be gone
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret.metadata.labels.as_ref();
    assert!(
        labels.is_none_or(|l| !l.contains_key("example.com/to-be-removed")),
        "previously applied template label should be removed from Secret after template removal"
    );

    // Operator-managed labels must remain
    let labels = labels.expect("Secret should still have operator labels");
    assert!(labels.contains_key("app.kubernetes.io/name"));
    assert!(labels.contains_key("app.kubernetes.io/instance"));
}

#[tokio::test]
async fn oauth2_secret_template_update_value() {
    let name = "test-st-update-val";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Update Value Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/env": "staging"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Update label value staging → prod
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "secretTemplate": {
                        "labels": {"example.com/env": "prod"}
                    }
                }
            })),
        )
        .await
        .unwrap();

    // True → False → True
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/env"),
        Some(&"prod".to_string()),
        "label value should be updated to prod"
    );
}

#[tokio::test]
async fn oauth2_secret_template_add_key() {
    let name = "test-st-add-key";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Add Key Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/key-a": "val-a"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Add a second key
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "secretTemplate": {
                        "labels": {
                            "example.com/key-a": "val-a",
                            "example.com/key-b": "val-b"
                        }
                    }
                }
            })),
        )
        .await
        .unwrap();

    // True → False → True
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/key-a"),
        Some(&"val-a".to_string()),
        "key-a should still be present"
    );
    assert_eq!(
        labels.get("example.com/key-b"),
        Some(&"val-b".to_string()),
        "newly added key-b should be present"
    );
}

#[tokio::test]
async fn oauth2_secret_template_remove_key() {
    let name = "test-st-remove-key";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Remove Key Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/key-a": "val-a",
                "example.com/key-b": "val-b"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Remove key-b using a JSON merge patch with an explicit null — the same patch that
    // client-side `kubectl apply` would compute from a 3-way merge when the desired manifest
    // omits key-b but the last-applied config included it.
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "secretTemplate": {
                        "labels": {
                            "example.com/key-b": null
                        }
                    }
                }
            })),
        )
        .await
        .unwrap();

    // True → False → True
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/key-a"),
        Some(&"val-a".to_string()),
        "key-a should still be present"
    );
    assert!(
        !labels.contains_key("example.com/key-b"),
        "key-b should be absent after removal (SSA releases ownership)"
    );
}

#[tokio::test]
async fn oauth2_secret_template_reclaim() {
    let name = "test-st-reclaim";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create with a secretTemplate annotation that would conflict with rotation,
    // but WITHOUT rotation enabled — so the template manager owns the annotation.
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Reclaim Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "annotations": {
                "kaniop.rs/rotation-enabled": "from-template"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Assert annotation owned by template manager with the template value
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"from-template".to_string()),
        "annotation should be set from secretTemplate when rotation is disabled"
    );

    // Enable rotation: the main operator re-applies the Secret with rotation annotations,
    // taking SSA ownership of kaniop.rs/rotation-enabled. filter_owned then discards the
    // conflicting template annotation; with nothing left to apply and nothing template-manager-
    // owned, needs_meta_template_apply returns None → SecretTemplateSynced stays True.
    //
    // Wait for SecretRotated=True: this condition only appears once secretRotation.enabled=true
    // has been processed, guaranteeing the Secret reflects the new rotation annotations.
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "spec": {
                    "secretRotation": {
                        "enabled": true,
                        "periodDays": 90
                    }
                }
            })),
        )
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretRotated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Main operator now owns the annotation with value "true"
    let secret = secret_api.get(&secret_name).await.unwrap();
    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");
    assert_eq!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"true".to_string()),
        "annotation should be owned by main operator with value 'true' when rotation is enabled"
    );
    assert_ne!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"from-template".to_string()),
        "template value should not win against main operator"
    );

    // NOTE: the reverse direction — disabling rotation and having the template manager
    // reclaim the annotation — is intentionally not tested here. When secretRotation is
    // removed from the spec the operator does not re-apply the Secret to
    // strip the rotation annotations; the Secret is only re-written on SecretInitialized=False
    // or a rotation event. The rotation annotations therefore remain on the Secret under the
    // main operator's SSA ownership until the next rotation cycle or a force-rotation.
    // If reclaim-on-disable is desired it would require a new reconcile branch that detects
    // stale rotation annotations (present on Secret, absent from spec) and triggers a re-apply.
}

// Conflict handling
#[tokio::test]
async fn oauth2_secret_template_conflict_partial() {
    let name = "test-st-conflict-partial";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Conflict Partial Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                // Conflicting: operator already owns this label
                "app.kubernetes.io/name": "my-custom-name",
                // Non-conflicting: template manager may own this
                "example.com/env": "production"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    let uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // A SecretTemplateConflict warning event must have been emitted
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid},reason=SecretTemplateConflict"
    ));
    check_event_with_timeout(&event_api, &opts).await;

    let event_list = event_api.list(&opts).await.unwrap();
    assert!(
        event_list.items.is_empty().not(),
        "SecretTemplateConflict event should be emitted"
    );
    let conflict_event = event_list.items.first().unwrap();
    assert!(
        conflict_event
            .message
            .as_deref()
            .unwrap_or("")
            .contains("app.kubernetes.io/name"),
        "event message should name the conflicting label key"
    );

    // Non-conflicting key must be present on Secret
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/env"),
        Some(&"production".to_string()),
        "non-conflicting label must be present on Secret"
    );

    // Conflicting key must hold the operator's value, not the user's value
    assert_ne!(
        labels.get("app.kubernetes.io/name"),
        Some(&"my-custom-name".to_string()),
        "conflicting label must not take the user's value"
    );
    assert!(
        labels.contains_key("app.kubernetes.io/name"),
        "operator's name label must still be present"
    );
}

#[tokio::test]
async fn oauth2_secret_template_conflict_all_keys() {
    let name = "test-st-conflict-all";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Every key in the template conflicts with an operator-owned label
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Conflict All Keys Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "app.kubernetes.io/name": "conflict-name",
                "app.kubernetes.io/instance": "conflict-instance",
                "app.kubernetes.io/managed-by": "conflict-managed"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    let uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    // SecretTemplateSynced=True immediately (filtered result is empty → in sync)
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // No SecretTemplateConflict event must be emitted
    // (needs_meta_template_apply returns None, reconcile block never entered)
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid},reason=SecretTemplateConflict"
    ));
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(
        event_list.items.is_empty(),
        "No SecretTemplateConflict event should be emitted when all keys are filtered"
    );

    // Operator's values must be unmodified
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_ne!(
        labels.get("app.kubernetes.io/name"),
        Some(&"conflict-name".to_string()),
        "operator label value must not be overwritten"
    );
    assert!(
        labels.contains_key("app.kubernetes.io/name"),
        "operator name label must be present"
    );
}

// SSA field manager separation
#[tokio::test]
async fn oauth2_secret_template_field_manager_separation() {
    let name = "test-st-managers";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Field Manager Separation Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/managed-by-template": "true"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Fetch Secret directly (not from store) to inspect managedFields
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    let managed_fields = secret
        .metadata
        .managed_fields
        .as_ref()
        .expect("Secret should have managedFields");

    let manager_names: Vec<&str> = managed_fields
        .iter()
        .filter_map(|e| e.manager.as_deref())
        .collect();

    assert!(
        manager_names.contains(&MAIN_MANAGER),
        "main operator manager '{MAIN_MANAGER}' must appear in managedFields, got: {manager_names:?}"
    );
    assert!(
        manager_names.contains(&TEMPLATE_MANAGER),
        "template manager '{TEMPLATE_MANAGER}' must appear in managedFields, got: {manager_names:?}"
    );

    // The two managers must be distinct entries
    let main_entry = managed_fields
        .iter()
        .find(|e| e.manager.as_deref() == Some(MAIN_MANAGER))
        .unwrap();
    let template_entry = managed_fields
        .iter()
        .find(|e| e.manager.as_deref() == Some(TEMPLATE_MANAGER))
        .unwrap();
    assert_ne!(
        main_entry.manager, template_entry.manager,
        "main and template managers should be distinct entries"
    );
}

#[tokio::test]
async fn oauth2_secret_template_manual_modification_overwritten() {
    let name = "test-st-manual-mod";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Manual Modification Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/env": "prod"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Manually change the template-owned label via a different field manager
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");

    let mut override_secret = Secret::default();
    override_secret.metadata.name = Some(secret_name.clone());
    override_secret.metadata.namespace = Some("default".to_string());
    override_secret.metadata.labels = Some(
        [("example.com/env".to_string(), "manual".to_string())]
            .into_iter()
            .collect(),
    );
    secret_api
        .patch(
            &secret_name,
            &PatchParams::apply("external-test-manager").force(),
            &Patch::Apply(&override_secret),
        )
        .await
        .unwrap();

    // Trigger reconcile by patching the OAuth2 client with a dummy annotation
    oauth2_api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(&json!({
                "metadata": {
                    "annotations": {
                        "test.kaniop.rs/trigger-reconcile": Timestamp::now().to_string()
                    }
                }
            })),
        )
        .await
        .unwrap();

    // Operator should detect drift and re-assert the template value
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/env"),
        Some(&"prod".to_string()),
        "template label value should be restored to 'prod' after drift correction"
    );
}

// Interaction with rotation
#[tokio::test]
async fn oauth2_secret_template_survives_rotation() {
    let name = "test-st-rotation-survives";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Survives Rotation Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretRotation": {
            "enabled": true,
            "periodDays": 90
        },
        "secretTemplate": {
            "labels": {
                "example.com/persist-across-rotation": "yes"
            }
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
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Force a secret rotation via annotation
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

    // Wait for rotation to complete
    wait_for(oauth2_api.clone(), name, is_oauth2_false("SecretRotated")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretRotated")).await;

    // After rotation, template labels must still be present
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels after rotation");
    assert_eq!(
        labels.get("example.com/persist-across-rotation"),
        Some(&"yes".to_string()),
        "template label must survive force rotation"
    );
}

#[tokio::test]
async fn oauth2_secret_template_coexists_with_rotation_annotations() {
    let name = "test-st-rotation-coexist";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Coexist With Rotation Annotations Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretRotation": {
            "enabled": true,
            "periodDays": 90
        },
        "secretTemplate": {
            "annotations": {
                "example.com/team": "platform"
            }
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
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .expect("Secret should have annotations");

    // Rotation annotations owned by main operator
    assert!(
        annotations.contains_key("kaniop.rs/rotation-enabled"),
        "rotation-enabled annotation must be present"
    );
    assert_eq!(
        annotations.get("kaniop.rs/rotation-enabled"),
        Some(&"true".to_string()),
        "rotation-enabled should be 'true'"
    );
    assert!(
        annotations.contains_key("kaniop.rs/rotation-period-days"),
        "rotation-period-days annotation must be present"
    );

    // Template annotation owned by template manager — must coexist
    assert_eq!(
        annotations.get("example.com/team"),
        Some(&"platform".to_string()),
        "template annotation must coexist with rotation annotations"
    );
}

// Edge cases
#[tokio::test]
async fn oauth2_secret_template_condition_absent_without_template() {
    let name = "test-st-no-template";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Create a standard confidential client with NO secretTemplate
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Absent Without Template Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    let current = oauth2_api.get(name).await.unwrap();
    let conditions = current.status.as_ref().and_then(|s| s.conditions.as_ref());
    assert!(
        conditions.is_none_or(|conds| conds.iter().all(|c| c.type_ != "SecretTemplateSynced")),
        "SecretTemplateSynced must be completely absent when no secretTemplate is set"
    );
}

#[tokio::test]
async fn oauth2_secret_template_public_client_no_condition() {
    let name = "test-st-public";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // Public client with secretTemplate set — no webhook prevents this
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Public Client Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "public": true,
        "secretTemplate": {
            "labels": {
                "example.com/public-template": "should-be-ignored"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    let uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    // Public clients have no secret, so we wait for ready (Exists + Updated)
    wait_for(oauth2_api.clone(), name, is_oauth2_ready()).await;

    // No Secret should be created for public clients
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret_result = secret_api.get(&secret_name).await;
    assert!(
        secret_result.is_err(),
        "No Secret should be created for public clients"
    );

    // SecretTemplateSynced must be absent for public clients
    let current = oauth2_api.get(name).await.unwrap();
    let conditions = current.status.as_ref().and_then(|s| s.conditions.as_ref());
    assert!(
        conditions.is_none_or(|conds| conds.iter().all(|c| c.type_ != "SecretTemplateSynced")),
        "SecretTemplateSynced must be absent for public clients"
    );

    // No SecretTemplateConflict event
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid},reason=SecretTemplateConflict"
    ));
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(
        event_list.items.is_empty(),
        "No SecretTemplateConflict event should be emitted for public clients"
    );
}

#[tokio::test]
async fn oauth2_secret_template_empty_template() {
    let name = "test-st-empty";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    // secretTemplate: {} — present but with no labels or annotations
    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Empty Template Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {}
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    let uid = oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap()
        .uid()
        .unwrap();

    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;
    // SecretTemplateSynced=True immediately (no keys → template manager owns nothing → in sync)
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // Secret must not have any extra labels/annotations beyond operator defaults
    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();

    // No unexpected annotations (beyond operator's own)
    let annotations = secret.metadata.annotations.as_ref();
    assert!(
        annotations.is_none_or(|a| !a.keys().any(|k| k.starts_with("example.com/"))),
        "Empty secretTemplate should not add any extra annotations"
    );

    // No unexpected labels beyond operator-managed ones
    let labels = secret.metadata.labels.as_ref();
    assert!(
        labels.is_none_or(|l| !l.keys().any(|k| k.starts_with("example.com/"))),
        "Empty secretTemplate should not add any extra labels"
    );

    // No SecretTemplateConflict event
    let event_api = Api::<Event>::namespaced(s.client.clone(), "default");
    let opts = ListParams::default().fields(&format!(
        "involvedObject.kind=KanidmOAuth2Client,involvedObject.apiVersion=kaniop.rs/v1beta1,involvedObject.uid={uid},reason=SecretTemplateConflict"
    ));
    let event_list = event_api.list(&opts).await.unwrap();
    assert!(
        event_list.items.is_empty(),
        "No SecretTemplateConflict event should be emitted for empty secretTemplate"
    );
}

#[tokio::test]
async fn oauth2_secret_template_condition_deferred_until_secret_exists() {
    let name = "test-st-deferred";
    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let oauth2_spec = json!({
        "kanidmRef": {"name": KANIDM_NAME},
        "displayname": "ST Deferred Until Secret Exists Test",
        "redirectUrl": [],
        "origin": format!("https://{name}.example.com"),
        "secretTemplate": {
            "labels": {
                "example.com/deferred-label": "value"
            }
        }
    });
    let oauth2 = KanidmOAuth2Client::new(name, serde_json::from_value(oauth2_spec).unwrap());
    let oauth2_api = Api::<KanidmOAuth2Client>::namespaced(s.client.clone(), "default");
    oauth2_api
        .create(&PostParams::default(), &oauth2)
        .await
        .unwrap();

    // Wait for SecretInitialized=True — the Secret now exists in the operator's watch store.
    // SecretTemplateSynced should NOT have appeared before this point (deferred behaviour),
    // but verifying that timing reliably in a test is impractical; instead we verify that:
    // 1. After SecretInitialized, the condition appears (False then True).
    // 2. By the time SecretTemplateSynced=True, the Secret has the correct labels.
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretInitialized")).await;

    // After SecretInitialized=True the template apply is triggered; condition reaches True.
    wait_for(
        oauth2_api.clone(),
        name,
        is_oauth2_false("SecretTemplateSynced"),
    )
    .await;
    wait_for(oauth2_api.clone(), name, is_oauth2("SecretTemplateSynced")).await;

    // At this point SecretInitialized must also be True (ordering guarantee)
    let current = oauth2_api.get(name).await.unwrap();
    let conditions = current.status.unwrap().conditions.unwrap_or_default();
    assert!(
        conditions
            .iter()
            .any(|c| c.type_ == "SecretInitialized" && c.status == "True"),
        "SecretInitialized must be True when SecretTemplateSynced is True"
    );

    let secret_name = format!("{name}-kanidm-oauth2-credentials");
    let secret_api = Api::<Secret>::namespaced(s.client.clone(), "default");
    let secret = secret_api.get(&secret_name).await.unwrap();
    let labels = secret
        .metadata
        .labels
        .as_ref()
        .expect("Secret should have labels");
    assert_eq!(
        labels.get("example.com/deferred-label"),
        Some(&"value".to_string()),
        "deferred template label should be applied after Secret is initialized"
    );
}
