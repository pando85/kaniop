use crate::admission::{AdmissionResponse, AdmissionReview};
use crate::backup_validator;
use crate::state::WebhookState;
use crate::validator::{HasKanidmRef, KanidmEntityKind, check_duplicate, check_external_duplicate};

use axum::extract::State;
use axum::response::Json;
use kaniop_backup::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
use kaniop_operator::kanidm::restore::KanidmRestore;
use kube::{Resource, ResourceExt};
use tracing::{debug, error};

/// Generic validation handler
pub async fn validate_resource<T>(
    state: &WebhookState,
    store: &kube::runtime::reflector::Store<T>,
    review: AdmissionReview<T>,
    resource_name: &str,
    entity_kind: KanidmEntityKind,
) -> Json<AdmissionReview<()>>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    // Only validate CREATE operations (kanidmRef and kanidmName are immutable)
    if operation != "CREATE" {
        debug!(
            "Skipping validation for {} operation on {}",
            operation, resource_name
        );
        return Json(review.response(AdmissionResponse::allow(uid)));
    }

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            error!("Missing object in CREATE request");
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    if let Err(err) = check_duplicate(object, resource_name, store) {
        debug!(
            "Validation failed for {} {}/{}: {}",
            resource_name,
            object.namespace().unwrap_or_else(|| "default".to_string()),
            object.name_any(),
            err
        );
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }

    if let Err(err) = check_external_duplicate(state, object, resource_name, entity_kind).await {
        debug!(
            "External entity validation failed for {} {}/{}: {}",
            resource_name,
            object.namespace().unwrap_or_else(|| "default".to_string()),
            object.name_any(),
            err
        );
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }

    debug!(
        "Validation passed for {} {}/{}",
        resource_name,
        object.namespace().unwrap_or_else(|| "default".to_string()),
        object.name_any()
    );
    Json(review.response(AdmissionResponse::allow(uid)))
}

// Concrete handlers for each resource type
pub async fn validate_kanidm_group(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_group::crd::KanidmGroup>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(
        &state,
        &state.group_store,
        review,
        "KanidmGroup",
        KanidmEntityKind::Group,
    )
    .await
}

pub async fn validate_kanidm_person(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_person::crd::KanidmPersonAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(
        &state,
        &state.person_store,
        review,
        "KanidmPersonAccount",
        KanidmEntityKind::Person,
    )
    .await
}

pub async fn validate_kanidm_oauth2(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_oauth2::crd::KanidmOAuth2Client>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(
        &state,
        &state.oauth2_store,
        review,
        "KanidmOAuth2Client",
        KanidmEntityKind::OAuth2Client,
    )
    .await
}

pub async fn validate_kanidm_service_account(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<kaniop_service_account::crd::KanidmServiceAccount>>,
) -> Json<AdmissionReview<()>> {
    validate_resource(
        &state,
        &state.service_account_store,
        review,
        "KanidmServiceAccount",
        KanidmEntityKind::ServiceAccount,
    )
    .await
}

pub async fn validate_backup_repository(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<KanidmBackupRepository>>,
) -> Json<AdmissionReview<()>> {
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    if operation == "CREATE" || operation == "UPDATE" {
        if let Err(err) =
            backup_validator::validate_repository_prefix_unique(object, &state.repository_store)
        {
            debug!(
                "Repository prefix validation failed for {}/{}: {}",
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any(),
                err
            );
            return Json(review.response(AdmissionResponse::deny(uid, err)));
        }
    }

    if operation == "UPDATE" {
        if let Some(old_object) = request.old_object.as_ref() {
            if let Err(err) =
                backup_validator::validate_repository_immutable_after_use(old_object, object)
            {
                return Json(review.response(AdmissionResponse::deny(uid, err)));
            }
        }
    }

    if !object.spec.s3.endpoint.starts_with("https://") && !object.spec.s3.insecure {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "Repository endpoint must use HTTPS",
        )));
    }

    if object.spec.s3.prefix.contains("..") {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "Repository prefix contains path traversal",
        )));
    }

    if let Err(err) = backup_validator::validate_auth_method_exactly_one(
        &object.spec.authentication.writer,
        "authentication.writer",
    ) {
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }
    if let Err(err) = backup_validator::validate_auth_method_exactly_one(
        &object.spec.authentication.reader,
        "authentication.reader",
    ) {
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }
    if let Err(err) = backup_validator::validate_auth_method_exactly_one(
        &object.spec.authentication.deleter,
        "authentication.deleter",
    ) {
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }

    debug!(
        "Validation passed for KanidmBackupRepository {}/{}",
        object.namespace().unwrap_or_else(|| "default".to_string()),
        object.name_any()
    );
    Json(review.response(AdmissionResponse::allow(uid)))
}

fn validate_cron_schedule(schedule: &str) -> std::result::Result<(), String> {
    use cron::Schedule;
    use std::str::FromStr;

    if Schedule::from_str(schedule).is_ok() {
        return Ok(());
    }
    let with_seconds = format!("0 {schedule}");
    if Schedule::from_str(&with_seconds).is_ok() {
        return Ok(());
    }
    Err(format!(
        "invalid cron schedule '{schedule}': must be a valid 5-field or 6-field cron expression"
    ))
}

pub async fn validate_backup_schedule(
    State(state): State<WebhookState>,
    Json(review): Json<AdmissionReview<KanidmBackupSchedule>>,
) -> Json<AdmissionReview<()>> {
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    if operation == "CREATE" || operation == "UPDATE" {
        if let Err(err) =
            backup_validator::validate_schedule_unique_kanidm_target(object, &state.schedule_store)
        {
            debug!(
                "Schedule unique target validation failed for {}/{}: {}",
                object.namespace().unwrap_or_else(|| "default".to_string()),
                object.name_any(),
                err
            );
            return Json(review.response(AdmissionResponse::deny(uid, err)));
        }
    }

    if operation == "UPDATE" {
        if let Some(old_object) = request.old_object.as_ref() {
            if let Err(err) =
                backup_validator::validate_schedule_immutable_after_discovery(old_object, object)
            {
                return Json(review.response(AdmissionResponse::deny(uid, err)));
            }
        }
    }

    if object.spec.schedule.is_empty() {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "Schedule cron expression is required",
        )));
    }

    if let Err(err) = validate_cron_schedule(&object.spec.schedule) {
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }

    if object.spec.concurrency_policy != "Forbid" {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "concurrencyPolicy must be Forbid",
        )));
    }

    debug!(
        "Validation passed for KanidmBackupSchedule {}/{}",
        object.namespace().unwrap_or_else(|| "default".to_string()),
        object.name_any()
    );
    Json(review.response(AdmissionResponse::allow(uid)))
}

pub async fn validate_backup(
    State(_state): State<WebhookState>,
    Json(review): Json<AdmissionReview<KanidmBackup>>,
) -> Json<AdmissionReview<()>> {
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    if operation == "UPDATE" {
        let old_object = match request.old_object.as_ref() {
            Some(obj) => obj,
            None => {
                return Json(review.response(AdmissionResponse::deny(
                    uid,
                    "Cannot validate update: missing oldObject",
                )));
            }
        };

        if let Err(err) = backup_validator::validate_backup_immutable_spec(old_object, object) {
            return Json(review.response(AdmissionResponse::deny(uid, err)));
        }
    }

    if object.spec.backup_id.is_empty() {
        return Json(review.response(AdmissionResponse::deny(uid, "backupId is required")));
    }

    if object.spec.manifest_key.is_empty() {
        return Json(review.response(AdmissionResponse::deny(uid, "manifestKey is required")));
    }

    if object.spec.manifest_key.contains("..") {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "manifestKey contains path traversal",
        )));
    }

    debug!(
        "Validation passed for KanidmBackup {}/{}",
        object.namespace().unwrap_or_else(|| "default".to_string()),
        object.name_any()
    );
    Json(review.response(AdmissionResponse::allow(uid)))
}

pub async fn validate_kanidm_restore(
    State(_state): State<WebhookState>,
    Json(review): Json<AdmissionReview<KanidmRestore>>,
) -> Json<AdmissionReview<()>> {
    let request = match review.request.as_ref() {
        Some(req) => req,
        None => {
            error!("Missing request in admission review");
            return Json(review.response(AdmissionResponse::deny(
                "unknown".to_string(),
                "Invalid admission review: missing request",
            )));
        }
    };

    let uid = request.uid.clone();
    let operation = &request.operation;

    if operation != "CREATE" && operation != "UPDATE" {
        debug!(
            "Skipping validation for {} operation on KanidmRestore",
            operation
        );
        return Json(review.response(AdmissionResponse::allow(uid)));
    }

    let object = match request.object.as_ref() {
        Some(obj) => obj,
        None => {
            return Json(review.response(AdmissionResponse::deny(
                uid,
                "Invalid admission review: missing object",
            )));
        }
    };

    let annotations = object
        .metadata
        .annotations
        .as_ref()
        .cloned()
        .unwrap_or_default();

    if let Err(err) = backup_validator::validate_break_glass_annotations(&annotations) {
        debug!(
            "Break-glass validation failed for KanidmRestore {}/{}: {}",
            object.namespace().unwrap_or_else(|| "default".to_string()),
            object.name_any(),
            err
        );
        return Json(review.response(AdmissionResponse::deny(uid, err)));
    }

    debug!(
        "Validation passed for KanidmRestore {}/{}",
        object.namespace().unwrap_or_else(|| "default".to_string()),
        object.name_any()
    );
    Json(review.response(AdmissionResponse::allow(uid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{AdmissionRequest, AdmissionReview};
    use kaniop_backup::crd::{BackupKanidmRef, BackupRepositoryRef, KanidmBackupSpec};
    use kube::api::ObjectMeta;

    fn test_backup(name: &str, backup_id: &str, manifest_key: &str) -> KanidmBackup {
        KanidmBackup {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupSpec {
                backup_id: backup_id.to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "corp-idm".to_string(),
                    uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
                manifest_key: manifest_key.to_string(),
            },
            status: None,
        }
    }

    fn test_admission_review(
        operation: &str,
        object: KanidmBackup,
        old_object: Option<KanidmBackup>,
    ) -> AdmissionReview<KanidmBackup> {
        AdmissionReview {
            api_version: "admission.k8s.io/v1".to_string(),
            kind: "AdmissionReview".to_string(),
            request: Some(AdmissionRequest {
                uid: "test-uid".to_string(),
                operation: operation.to_string(),
                object: Some(object),
                old_object,
            }),
            response: None,
        }
    }

    #[test]
    fn admission_request_deserializes_with_old_object() {
        let json = serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "operation": "UPDATE",
                "object": {
                    "metadata": {"name": "kb-test", "namespace": "default"},
                    "spec": {
                        "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
                        "kanidmRef": {"name": "corp-idm", "uid": "uid-123"},
                        "repositoryRef": {"name": "offsite"},
                        "manifestKey": "v1/manifest.json"
                    }
                },
                "oldObject": {
                    "metadata": {"name": "kb-test", "namespace": "default"},
                    "spec": {
                        "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
                        "kanidmRef": {"name": "corp-idm", "uid": "uid-123"},
                        "repositoryRef": {"name": "offsite"},
                        "manifestKey": "v1/old-manifest.json"
                    }
                }
            }
        });

        let review: AdmissionReview<KanidmBackup> = serde_json::from_value(json).unwrap();
        let request = review.request.unwrap();
        assert_eq!(request.operation, "UPDATE");
        assert!(request.old_object.is_some());
        let old = request.old_object.unwrap();
        assert_eq!(old.spec.manifest_key, "v1/old-manifest.json");
    }

    #[test]
    fn admission_request_deserializes_without_old_object_for_create() {
        let json = serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "operation": "CREATE",
                "object": {
                    "metadata": {"name": "kb-test", "namespace": "default"},
                    "spec": {
                        "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
                        "kanidmRef": {"name": "corp-idm", "uid": "uid-123"},
                        "repositoryRef": {"name": "offsite"},
                        "manifestKey": "v1/manifest.json"
                    }
                }
            }
        });

        let review: AdmissionReview<KanidmBackup> = serde_json::from_value(json).unwrap();
        let request = review.request.unwrap();
        assert_eq!(request.operation, "CREATE");
        assert!(request.old_object.is_none());
    }

    #[test]
    fn backup_update_with_changed_spec_is_denied_by_validator() {
        let old = test_backup(
            "kb-test",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "v1/old.json",
        );
        let mut new = old.clone();
        new.spec.manifest_key = "v1/new.json".to_string();

        let result = backup_validator::validate_backup_immutable_spec(&old, &new);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("immutable"));
    }

    #[test]
    fn backup_update_with_only_metadata_changes_is_allowed_by_validator() {
        let old = test_backup(
            "kb-test",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "v1/manifest.json",
        );
        let mut new = old.clone();
        new.metadata.labels = Some(std::collections::BTreeMap::from([(
            "app".to_string(),
            "test".to_string(),
        )]));

        let result = backup_validator::validate_backup_immutable_spec(&old, &new);
        assert!(result.is_ok());
    }

    #[test]
    fn backup_create_allows_any_spec() {
        let backup = test_backup(
            "kb-test",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "v1/manifest.json",
        );
        let review = test_admission_review("CREATE", backup, None);

        assert!(review.request.as_ref().unwrap().old_object.is_none());
        assert_eq!(review.request.as_ref().unwrap().operation, "CREATE");
    }

    #[test]
    fn backup_delete_is_allowed() {
        let backup = test_backup(
            "kb-test",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "v1/manifest.json",
        );
        let review = test_admission_review("DELETE", backup, None);

        assert_eq!(review.request.as_ref().unwrap().operation, "DELETE");
    }

    #[test]
    fn validate_cron_schedule_accepts_standard_5_field() {
        assert!(super::validate_cron_schedule("0 0 * * *").is_ok());
        assert!(super::validate_cron_schedule("*/15 * * * *").is_ok());
        assert!(super::validate_cron_schedule("0 0 1 JAN *").is_ok());
        assert!(super::validate_cron_schedule("0 0 * * MON-FRI").is_ok());
    }

    #[test]
    fn validate_cron_schedule_accepts_6_field_with_seconds() {
        assert!(super::validate_cron_schedule("0 0 0 * * *").is_ok());
        assert!(super::validate_cron_schedule("0 */15 * * * *").is_ok());
    }

    #[test]
    fn validate_cron_schedule_rejects_invalid() {
        assert!(super::validate_cron_schedule("not-a-cron").is_err());
        assert!(super::validate_cron_schedule("60 * * * *").is_err());
        assert!(super::validate_cron_schedule("@@@@@").is_err());
        assert!(super::validate_cron_schedule("").is_err());
    }
}
