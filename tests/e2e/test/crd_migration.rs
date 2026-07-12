use super::{
    init_crypto_provider, poll_until, setup_kanidm_connection, stabilization_delay, wait_for,
};

use kaniop_person::crd::KanidmPersonAccount;

use std::ops::Not;

use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{DynamicObject, Patch, PatchParams};
use kube::{Api, api::ListParams, runtime::wait::Condition};
use kube::{Client, ResourceExt};

const KANIDM_NAME: &str = "test-migration";
const LEGACY_CRD_NAME: &str = "kanidmpersonsaccounts.kaniop.rs";
const CORRECTED_CRD_NAME: &str = "kanidmpersonaccounts.kaniop.rs";
const LEGACY_PLURAL: &str = "kanidmpersonsaccounts";
const LEGACY_FINALIZER: &str = "kanidmpersonsaccounts.kaniop.rs/finalizer";
const MIGRATION_MARKER_NAME: &str = "kaniop-person-crd-migration";
const MIGRATION_NAMESPACE: &str = "kaniop";
const BACKUP_LABEL_SELECTOR: &str = "kaniop.rs/migration=person-plural-v1";
const PRE_MIGRATION_UID_ANNOTATION: &str = "kaniop.rs/pre-migration-uid";
const PRE_MIGRATION_KANIDM_UUID_ANNOTATION: &str = "kaniop.rs/pre-migration-kanidm-uuid";
const POST_MIGRATION_UID_ANNOTATION: &str = "kaniop.rs/post-migration-uid";
const PERSON_NAME_PREFIX: &str = "test-migration-person-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationStage {
    PreMigration,
    PostMigration,
    IdempotentRerun,
}

fn migration_stage() -> MigrationStage {
    init_crypto_provider();
    match std::env::var("MIGRATION_STAGE").ok().as_deref() {
        Some("pre-migration") => MigrationStage::PreMigration,
        Some("post-migration") | None => MigrationStage::PostMigration,
        Some("idempotent-rerun") => MigrationStage::IdempotentRerun,
        Some(stage) => panic!("unknown MIGRATION_STAGE: {stage}"),
    }
}

fn is_person_condition(cond: &str, status: String) -> impl Condition<KanidmPersonAccount> + '_ {
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

fn is_person_ready() -> impl Condition<KanidmPersonAccount> {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|person| person.status.as_ref())
            .is_some_and(|status| status.ready)
    }
}

fn is_crd_established() -> impl Condition<CustomResourceDefinition> {
    |obj: Option<&CustomResourceDefinition>| {
        obj.and_then(|crd| crd.status.as_ref())
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == "Established" && c.status == "True")
            })
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn migration_source_count() -> usize {
    env_var("MIGRATION_SOURCE_COUNT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

fn migration_marker_phase() -> String {
    env_var("MIGRATION_EXPECTED_PHASE").unwrap_or_else(|| "Completed".to_string())
}

fn legacy_person_api_resource() -> kube::api::ApiResource {
    kube::api::ApiResource {
        group: "kaniop.rs".to_string(),
        version: "v1beta1".to_string(),
        api_version: "kaniop.rs/v1beta1".to_string(),
        kind: "KanidmPersonAccount".to_string(),
        plural: LEGACY_PLURAL.to_string(),
    }
}

async fn get_crd(client: &Client, name: &str) -> Option<CustomResourceDefinition> {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    crd_api.get(name).await.ok()
}

async fn wait_for_crd_established(client: &Client, name: &str) {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    wait_for(crd_api, name, is_crd_established()).await;
}

async fn wait_for_crd_gone(client: &Client, name: &str) {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    poll_until(format!("{name} CRD removed").as_str(), || {
        let crd_api = crd_api.clone();
        let name = name.to_string();
        async move {
            if crd_api.get(&name).await.is_err() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;
}

async fn get_migration_marker(client: &Client) -> Option<ConfigMap> {
    let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), MIGRATION_NAMESPACE);
    cm_api.get(MIGRATION_MARKER_NAME).await.ok()
}

async fn count_backup_secrets(client: &Client) -> usize {
    let secret_api: Api<Secret> = Api::namespaced(client.clone(), MIGRATION_NAMESPACE);
    let lp = ListParams::default().labels(BACKUP_LABEL_SELECTOR);
    secret_api
        .list(&lp)
        .await
        .map(|l| l.items.len())
        .unwrap_or(0)
}

async fn list_migration_persons(client: &Client, namespace: &str) -> Vec<KanidmPersonAccount> {
    let person_api: Api<KanidmPersonAccount> = Api::namespaced(client.clone(), namespace);
    person_api
        .list(&ListParams::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|p| p.name_any().starts_with(PERSON_NAME_PREFIX))
        .collect()
}

async fn list_legacy_persons(client: &Client) -> Vec<DynamicObject> {
    let ar = legacy_person_api_resource();
    let legacy_api: Api<DynamicObject> = Api::all_with(client.clone(), &ar);
    legacy_api
        .list(&ListParams::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|p| p.name_any().starts_with(PERSON_NAME_PREFIX))
        .collect()
}

async fn annotate_legacy_person(
    client: &Client,
    namespace: &str,
    name: &str,
    key: &str,
    value: &str,
) {
    let ar = legacy_person_api_resource();
    let legacy_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, &ar);
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                key: value
            }
        }
    });
    legacy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .unwrap_or_else(|e| {
            panic!("failed to annotate legacy person {name} with {key}={value}: {e}")
        });
}

async fn annotate_corrected_person(
    client: &Client,
    namespace: &str,
    name: &str,
    key: &str,
    value: &str,
) {
    let person_api: Api<KanidmPersonAccount> = Api::namespaced(client.clone(), namespace);
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                key: value
            }
        }
    });
    person_api
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .unwrap_or_else(|e| {
            panic!("failed to annotate corrected person {name} with {key}={value}: {e}")
        });
}

e2e_test!(crd_migration_record_pre_migration_identity, {
    let stage = migration_stage();
    assert_eq!(
        stage,
        MigrationStage::PreMigration,
        "this test must run in MIGRATION_STAGE=pre-migration"
    );

    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let legacy_persons = list_legacy_persons(&s.client).await;
    assert!(
        legacy_persons.is_empty().not(),
        "no legacy persons found to record pre-migration identity"
    );

    for person in &legacy_persons {
        let name = person.name_any();
        let namespace = person.namespace().unwrap_or_else(|| "default".to_string());
        let k8s_uid = person
            .uid()
            .unwrap_or_else(|| panic!("legacy person {name} has no UID"));

        let kanidm_account = s
            .kanidm_client
            .idm_person_account_get(&name)
            .await
            .unwrap_or_else(|e| panic!("Kanidm get failed for {name}: {e:?}"));
        assert!(
            kanidm_account.is_some(),
            "Kanidm account {name} must exist before migration"
        );

        let account = kanidm_account.unwrap();
        let kanidm_uuid = account
            .attrs
            .get("uuid")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| panic!("Kanidm account {name} has no uuid attribute"));

        assert!(
            kanidm_uuid.is_empty().not(),
            "Kanidm uuid for {name} must not be empty"
        );

        annotate_legacy_person(
            &s.client,
            &namespace,
            &name,
            PRE_MIGRATION_UID_ANNOTATION,
            &k8s_uid,
        )
        .await;
        annotate_legacy_person(
            &s.client,
            &namespace,
            &name,
            PRE_MIGRATION_KANIDM_UUID_ANNOTATION,
            &kanidm_uuid,
        )
        .await;

        eprintln!(
            "Recorded pre-migration identity for {name}: k8s_uid={k8s_uid}, kanidm_uuid={kanidm_uuid}"
        );
    }
});

e2e_test!(crd_migration_legacy_crd_removed, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();
    wait_for_crd_gone(&client, LEGACY_CRD_NAME).await;
});

e2e_test!(crd_migration_corrected_crd_established, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();
    let crd = get_crd(&client, CORRECTED_CRD_NAME).await;
    assert!(
        crd.is_some(),
        "corrected CRD {CORRECTED_CRD_NAME} must exist after migration"
    );
    wait_for_crd_established(&client, CORRECTED_CRD_NAME).await;
});

e2e_test!(crd_migration_marker_completed, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let expected_phase = migration_marker_phase();
    assert_eq!(
        expected_phase, "Completed",
        "marker phase must be Completed in post-migration/idempotent-rerun stage, got: {expected_phase}"
    );

    let client = Client::try_default().await.unwrap();
    let marker = poll_until("migration marker phase=Completed", || {
        let client = client.clone();
        async move {
            let marker = get_migration_marker(&client).await?;
            let data = marker.data.clone()?;
            let phase = data.get("phase")?.clone();
            if phase == "Completed" {
                Some(marker)
            } else {
                None
            }
        }
    })
    .await;

    let data = marker.data.expect("marker must have data");
    let version = data
        .get("migrationVersion")
        .expect("must have migrationVersion");
    assert_eq!(version, "person-plural-v1");

    let legacy = data.get("legacyCRD").expect("must have legacyCRD");
    assert_eq!(legacy, LEGACY_CRD_NAME);

    let corrected = data.get("correctedCRD").expect("must have correctedCRD");
    assert_eq!(corrected, CORRECTED_CRD_NAME);

    let source_count = data.get("sourceCount").expect("must have sourceCount");
    let source: usize = source_count.parse().expect("sourceCount must be numeric");
    assert!(source > 0, "sourceCount must be > 0, got {source}");
});

e2e_test!(crd_migration_persons_restored_with_correct_specs, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();
    let expected_count = migration_source_count();

    let migration_persons = list_migration_persons(&client, "default").await;

    assert!(
        migration_persons.len() >= expected_count,
        "expected at least {expected_count} migrated persons, found {}",
        migration_persons.len()
    );

    for person in &migration_persons {
        assert!(
            person.spec.person_attributes.displayname.is_empty().not(),
            "person {} must have a displayname after migration",
            person.name_any()
        );
    }
});

e2e_test!(crd_migration_persons_reconcile_after_migration, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let migration_persons = list_migration_persons(&s.client, "default").await;

    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found to verify reconciliation"
    );

    for person in &migration_persons {
        let name = person.name_any();
        let person_api: Api<KanidmPersonAccount> = Api::namespaced(s.client.clone(), "default");
        wait_for(
            person_api.clone(),
            &name,
            is_person_condition("Exists", "True".to_string()),
        )
        .await;
        wait_for(
            person_api.clone(),
            &name,
            is_person_condition("Updated", "True".to_string()),
        )
        .await;
        wait_for(person_api.clone(), &name, is_person_ready()).await;

        let kanidm_person = s.kanidm_client.idm_person_account_get(&name).await.unwrap();
        assert!(
            kanidm_person.is_some(),
            "Kanidm account for {name} must exist after migration"
        );
    }
});

e2e_test!(crd_migration_kanidm_uuid_preserved, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let migration_persons = list_migration_persons(&s.client, "default").await;
    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found to verify Kanidm UUID preservation"
    );

    let mut checked = 0;
    for person in &migration_persons {
        let name = person.name_any();

        let stored_uuid = person
            .annotations()
            .get(PRE_MIGRATION_KANIDM_UUID_ANNOTATION)
            .cloned()
            .unwrap_or_default();

        assert!(
            stored_uuid.is_empty().not(),
            "person {name} must have {PRE_MIGRATION_KANIDM_UUID_ANNOTATION} annotation \
             set by pre-migration recording; without it we cannot prove Kanidm identity \
             was preserved"
        );

        let kanidm_account = s
            .kanidm_client
            .idm_person_account_get(&name)
            .await
            .unwrap_or_else(|e| panic!("Kanidm get failed for {name}: {e:?}"));
        assert!(
            kanidm_account.is_some(),
            "Kanidm account {name} must survive migration"
        );

        let account = kanidm_account.unwrap();
        let current_uuid = account
            .attrs
            .get("uuid")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| {
                panic!("Kanidm account {name} has no uuid attribute after migration")
            });

        assert_eq!(
            current_uuid, stored_uuid,
            "Kanidm UUID for {name} CHANGED during migration: was {stored_uuid}, now {current_uuid}. \
             This means the Kanidm account was deleted and recreated, which is a migration failure."
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "at least one person must have been checked for Kanidm UUID preservation"
    );
});

e2e_test!(crd_migration_metadata_labels_annotations_preserved, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();

    let migration_persons = list_migration_persons(&client, "default").await;
    let labeled_person = migration_persons
        .iter()
        .find(|p| p.name_any() == "test-migration-person-labeled");

    assert!(
        labeled_person.is_some(),
        "test-migration-person-labeled must exist after migration"
    );

    let person = labeled_person.unwrap();
    let labels = person.labels();
    assert!(
        labels.get("migration-test").is_some_and(|v| v == "true"),
        "label migration-test=true must survive migration, got: {labels:?}"
    );
    assert!(
        labels.get("team").is_some_and(|v| v == "platform"),
        "label team=platform must survive migration, got: {labels:?}"
    );

    let annotations = person.annotations();
    assert!(
        annotations
            .get("migration-test-annotation")
            .is_some_and(|v| v == "preserved"),
        "annotation migration-test-annotation must survive migration, got: {annotations:?}"
    );
});

e2e_test!(crd_migration_argocd_tracking_preserved, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();

    let migration_persons = list_migration_persons(&client, "default").await;
    let argocd_person = migration_persons
        .iter()
        .find(|p| p.name_any() == "test-migration-person-argocd");

    assert!(
        argocd_person.is_some(),
        "test-migration-person-argocd must exist after migration"
    );

    let person = argocd_person.unwrap();
    let annotations = person.annotations();
    assert!(
        annotations.get("argocd.argoproj.io/tracking-id").is_some(),
        "Argo CD tracking annotation must survive migration, got: {annotations:?}"
    );
});

e2e_test!(crd_migration_uid_changed, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();

    let migration_persons = list_migration_persons(&client, "default").await;
    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found to verify UID change"
    );

    let mut checked = 0;
    for person in &migration_persons {
        let pre_uid = person
            .annotations()
            .get(PRE_MIGRATION_UID_ANNOTATION)
            .cloned()
            .unwrap_or_default();
        let current_uid = person.uid().unwrap_or_default();

        assert!(
            pre_uid.is_empty().not(),
            "person {} must have {PRE_MIGRATION_UID_ANNOTATION} annotation",
            person.name_any()
        );
        assert_ne!(
            pre_uid,
            current_uid.to_string(),
            "UID for {} should have changed during migration",
            person.name_any()
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "at least one person must have been checked for UID change"
    );
});

e2e_test!(crd_migration_legacy_finalizer_retained, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();

    let migration_persons = list_migration_persons(&client, "default").await;
    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found to verify finalizer"
    );

    let person_api: Api<KanidmPersonAccount> = Api::namespaced(client.clone(), "default");

    tokio::time::sleep(stabilization_delay()).await;

    for person in &migration_persons {
        let name = person.name_any();
        let current = person_api.get(&name).await.unwrap();
        let finalizers = current.finalizers();

        assert!(
            finalizers.iter().any(|f| f == LEGACY_FINALIZER),
            "person {name} must have the legacy finalizer {LEGACY_FINALIZER} \
             re-added by the new operator (plan: finalizer string is not changed); \
             got: {finalizers:?}"
        );
    }
});

e2e_test!(crd_migration_backup_secrets_cleaned_after_verification, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let expected_phase = migration_marker_phase();
    assert_eq!(
        expected_phase, "Completed",
        "backup cleanup check requires Completed phase"
    );

    let client = Client::try_default().await.unwrap();
    tokio::time::sleep(stabilization_delay()).await;
    let backup_count = count_backup_secrets(&client).await;
    assert_eq!(
        backup_count, 0,
        "backup secrets should be removed after successful verification, found {backup_count}"
    );
});

e2e_test!(crd_migration_idempotent_rerun, {
    let stage = migration_stage();
    assert_eq!(
        stage,
        MigrationStage::IdempotentRerun,
        "this test must run in MIGRATION_STAGE=idempotent-rerun"
    );

    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let migration_persons = list_migration_persons(&s.client, "default").await;
    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found for idempotency check"
    );

    for person in &migration_persons {
        let name = person.name_any();

        let recorded_uid = person
            .annotations()
            .get(POST_MIGRATION_UID_ANNOTATION)
            .cloned()
            .unwrap_or_default();

        assert!(
            recorded_uid.is_empty().not(),
            "person {name} must have {POST_MIGRATION_UID_ANNOTATION} annotation \
             set before the second helm upgrade; without it we cannot prove UID stability"
        );

        let current_uid = person.uid().unwrap_or_default();
        assert_eq!(
            recorded_uid,
            current_uid.to_string(),
            "person {name} UID changed between first and second helm upgrade: \
             was {recorded_uid}, now {current_uid}. Idempotency failed."
        );

        let stored_kanidm_uuid = person
            .annotations()
            .get(PRE_MIGRATION_KANIDM_UUID_ANNOTATION)
            .cloned()
            .unwrap_or_default();
        assert!(
            stored_kanidm_uuid.is_empty().not(),
            "person {name} must still have {PRE_MIGRATION_KANIDM_UUID_ANNOTATION} after rerun"
        );

        let kanidm_account = s
            .kanidm_client
            .idm_person_account_get(&name)
            .await
            .unwrap_or_else(|e| panic!("Kanidm get failed for {name}: {e:?}"));
        assert!(
            kanidm_account.is_some(),
            "Kanidm account {name} must still exist after rerun"
        );

        let current_kanidm_uuid = kanidm_account
            .unwrap()
            .attrs
            .get("uuid")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| panic!("Kanidm account {name} has no uuid after rerun"));

        assert_eq!(
            current_kanidm_uuid, stored_kanidm_uuid,
            "Kanidm UUID for {name} changed between first and second helm upgrade: \
             was {stored_kanidm_uuid}, now {current_kanidm_uuid}"
        );
    }
});

e2e_test!(crd_migration_record_post_migration_uids, {
    let stage = migration_stage();
    assert_eq!(
        stage,
        MigrationStage::PostMigration,
        "this test must run in MIGRATION_STAGE=post-migration (first pass only)"
    );

    let client = Client::try_default().await.unwrap();

    let migration_persons = list_migration_persons(&client, "default").await;
    assert!(
        migration_persons.is_empty().not(),
        "no migrated persons found to record post-migration UIDs"
    );

    for person in &migration_persons {
        let name = person.name_any();
        let namespace = person.namespace().unwrap_or_else(|| "default".to_string());
        let current_uid = person
            .uid()
            .unwrap_or_else(|| panic!("person {name} has no UID"));

        annotate_corrected_person(
            &client,
            &namespace,
            &name,
            POST_MIGRATION_UID_ANNOTATION,
            &current_uid,
        )
        .await;

        eprintln!("Recorded post-migration UID for {name}: {current_uid}");
    }
});

e2e_test!(crd_migration_zero_person_fresh_install, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let client = Client::try_default().await.unwrap();

    let crd = get_crd(&client, CORRECTED_CRD_NAME).await;
    assert!(
        crd.is_some(),
        "corrected CRD must exist even with zero persons"
    );

    let legacy = get_crd(&client, LEGACY_CRD_NAME).await;
    assert!(
        legacy.is_none(),
        "legacy CRD must not exist after fresh install or migration"
    );
});

e2e_test!(crd_migration_person_spec_roundtrip, {
    let stage = migration_stage();
    assert_ne!(
        stage,
        MigrationStage::PreMigration,
        "this test must not run in pre-migration stage"
    );

    let s = setup_kanidm_connection(KANIDM_NAME).await;

    let migration_persons = list_migration_persons(&s.client, "default").await;
    if migration_source_count() >= 4 {
        let person = migration_persons
            .iter()
            .find(|p| p.name_any() == "test-migration-person-posix")
            .expect("test-migration-person-posix must exist when MIGRATION_SOURCE_COUNT >= 4");
        let kanidm_account = s
            .kanidm_client
            .idm_person_account_get(&person.name_any())
            .await
            .unwrap();

        assert!(
            kanidm_account.is_some(),
            "Kanidm account for posix person must exist after migration"
        );

        let account = kanidm_account.unwrap();
        let expected_mail = person
            .spec
            .person_attributes
            .mail
            .as_ref()
            .and_then(|mail| mail.first())
            .cloned()
            .unwrap_or_default();
        assert!(
            expected_mail.is_empty().not(),
            "posix person must have a mail attribute"
        );

        let kanidm_mails = account.attrs.get("mail").cloned().unwrap_or_default();
        assert!(
            kanidm_mails.contains(&expected_mail),
            "mail {expected_mail} not found in Kanidm account after migration"
        );
    }
});
