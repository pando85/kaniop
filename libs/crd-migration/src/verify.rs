use std::time::Duration;

use kube::Api;
use kube::api::DynamicObject;
use serde_json::Value;
use tokio::time::sleep;

use crate::{
    API_GROUP, API_VERSION, KIND, LEGACY_FINALIZER, MigrationError, Result,
    backup::list_backup_entries, checksum::object_checksum,
};

const POLL_INTERVAL_SECS: u64 = 2;

pub struct StructuralVerificationResult {
    pub corrected_crd_established: bool,
    pub legacy_crd_absent: bool,
    pub source_count: usize,
    pub restored_count: usize,
    pub checksum_matches: usize,
    pub checksum_mismatches: Vec<String>,
    pub missing_restorations: Vec<String>,
}

pub struct AdoptionVerificationResult {
    pub corrected_crd_established: bool,
    pub legacy_crd_absent: bool,
    pub source_count: usize,
    pub restored_count: usize,
    pub checksum_matches: usize,
    pub checksum_mismatches: Vec<String>,
    pub missing_restorations: Vec<String>,
    pub missing_finalizers: Vec<String>,
    pub missing_exists: Vec<String>,
}

fn corrected_person_api_resource() -> kube::api::ApiResource {
    kube::api::ApiResource {
        group: API_GROUP.to_string(),
        version: API_VERSION.to_string(),
        api_version: format!("{API_GROUP}/{API_VERSION}"),
        kind: KIND.to_string(),
        plural: crate::CORRECTED_PLURAL.to_string(),
    }
}

pub async fn verify_structural(
    client: &kube::Client,
    backup_api: &Api<k8s_openapi::api::core::v1::Secret>,
    marker: &crate::state::MigrationMarker,
) -> Result<StructuralVerificationResult> {
    let backups = list_backup_entries(backup_api).await?;

    if backups.len() != marker.source_count {
        return Err(MigrationError::Verify(format!(
            "backup count {} does not match marker source_count {}",
            backups.len(),
            marker.source_count
        )));
    }

    let checksum_entries: Vec<_> = backups
        .iter()
        .map(|b| (b.namespace.clone(), b.name.clone(), b.checksum.clone()))
        .collect();
    let computed_set_checksum = crate::checksum::source_set_checksum(&checksum_entries);

    if computed_set_checksum != marker.source_set_checksum {
        return Err(MigrationError::Verify(format!(
            "source set checksum mismatch: computed {} does not match marker {}",
            computed_set_checksum, marker.source_set_checksum
        )));
    }

    check_structural_state(client, backup_api, &backups, marker).await
}

async fn check_structural_state(
    client: &kube::Client,
    backup_api: &Api<k8s_openapi::api::core::v1::Secret>,
    backups: &[crate::backup::BackupEntry],
    _marker: &crate::state::MigrationMarker,
) -> Result<StructuralVerificationResult> {
    let crd_api: Api<
        k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    > = Api::all(client.clone());

    let legacy_present = crate::crd::get_crd(&crd_api, crate::LEGACY_CRD_NAME)
        .await?
        .is_some();
    let corrected_crd = crate::crd::get_crd(&crd_api, crate::CORRECTED_CRD_NAME).await?;
    let corrected_established = corrected_crd
        .as_ref()
        .is_some_and(crate::crd::is_crd_established);

    let source_count = backups.len();
    let mut restored_count = 0;
    let mut checksum_matches = 0;
    let mut checksum_mismatches = Vec::new();
    let mut missing_restorations = Vec::new();

    for entry in backups {
        let ns_api: Api<DynamicObject> = Api::namespaced_with(
            client.clone(),
            &entry.namespace,
            &corrected_person_api_resource(),
        );

        match ns_api.get(&entry.name).await {
            Ok(restored) => {
                restored_count += 1;

                let restored_value: Value = serde_json::to_value(&restored).map_err(|e| {
                    MigrationError::Serialization(
                        format!(
                            "serialize restored object {}/{}",
                            entry.namespace, entry.name
                        ),
                        e,
                    )
                })?;

                let backup_obj = get_backup_object(backup_api, &entry.secret_name).await?;
                let backup_value = backup_obj.ok_or_else(|| {
                    MigrationError::Backup(format!(
                        "backup secret {} missing object.json",
                        entry.secret_name
                    ))
                })?;

                if metadata_spec_checksum_match(&backup_value, &restored_value) {
                    checksum_matches += 1;
                } else {
                    checksum_mismatches.push(format!("{}/{}", entry.namespace, entry.name));
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                missing_restorations.push(format!("{}/{}", entry.namespace, entry.name));
            }
            Err(e) => {
                return Err(MigrationError::Kube(
                    format!("get restored object {}/{}", entry.namespace, entry.name),
                    Box::new(e),
                ));
            }
        }
    }

    Ok(StructuralVerificationResult {
        corrected_crd_established: corrected_established,
        legacy_crd_absent: !legacy_present,
        source_count,
        restored_count,
        checksum_matches,
        checksum_mismatches,
        missing_restorations,
    })
}

pub fn structural_verification_passed(result: &StructuralVerificationResult) -> bool {
    result.corrected_crd_established
        && result.legacy_crd_absent
        && result.source_count == result.restored_count
        && result.checksum_matches == result.source_count
        && result.checksum_mismatches.is_empty()
        && result.missing_restorations.is_empty()
}

pub async fn verify_adoption(
    client: &kube::Client,
    backup_api: &Api<k8s_openapi::api::core::v1::Secret>,
    marker: &crate::state::MigrationMarker,
    timeout: Duration,
) -> Result<AdoptionVerificationResult> {
    let backups = list_backup_entries(backup_api).await?;

    if backups.len() != marker.source_count {
        return Err(MigrationError::Verify(format!(
            "backup count {} does not match marker source_count {}",
            backups.len(),
            marker.source_count
        )));
    }

    let checksum_entries: Vec<_> = backups
        .iter()
        .map(|b| (b.namespace.clone(), b.name.clone(), b.checksum.clone()))
        .collect();
    let computed_set_checksum = crate::checksum::source_set_checksum(&checksum_entries);

    if computed_set_checksum != marker.source_set_checksum {
        return Err(MigrationError::Verify(format!(
            "source set checksum mismatch: computed {} does not match marker {}",
            computed_set_checksum, marker.source_set_checksum
        )));
    }

    if backups.is_empty() {
        let crd_api: Api<
            k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
        > = Api::all(client.clone());
        let legacy_present = crate::crd::get_crd(&crd_api, crate::LEGACY_CRD_NAME)
            .await?
            .is_some();
        let corrected_crd = crate::crd::get_crd(&crd_api, crate::CORRECTED_CRD_NAME).await?;
        let corrected_established = corrected_crd.as_ref().is_some_and(crate::crd::is_crd_ready);

        return Ok(AdoptionVerificationResult {
            corrected_crd_established: corrected_established,
            legacy_crd_absent: !legacy_present,
            source_count: 0,
            restored_count: 0,
            checksum_matches: 0,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec![],
        });
    }

    let start = std::time::Instant::now();
    loop {
        let result = check_adoption_state(client, backup_api, &backups).await?;

        let all_adopted = result.missing_restorations.is_empty()
            && result.missing_finalizers.is_empty()
            && result.missing_exists.is_empty()
            && result.checksum_mismatches.is_empty()
            && result.checksum_matches == result.source_count;

        if all_adopted || start.elapsed() > timeout {
            return Ok(result);
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn check_adoption_state(
    client: &kube::Client,
    backup_api: &Api<k8s_openapi::api::core::v1::Secret>,
    backups: &[crate::backup::BackupEntry],
) -> Result<AdoptionVerificationResult> {
    let crd_api: Api<
        k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    > = Api::all(client.clone());

    let legacy_present = crate::crd::get_crd(&crd_api, crate::LEGACY_CRD_NAME)
        .await?
        .is_some();
    let corrected_crd = crate::crd::get_crd(&crd_api, crate::CORRECTED_CRD_NAME).await?;
    let corrected_established = corrected_crd.as_ref().is_some_and(crate::crd::is_crd_ready);

    let source_count = backups.len();
    let mut restored_count = 0;
    let mut checksum_matches = 0;
    let mut checksum_mismatches = Vec::new();
    let mut missing_restorations = Vec::new();
    let mut missing_finalizers = Vec::new();
    let mut missing_exists = Vec::new();

    for entry in backups {
        let ns_api: Api<DynamicObject> = Api::namespaced_with(
            client.clone(),
            &entry.namespace,
            &corrected_person_api_resource(),
        );

        match ns_api.get(&entry.name).await {
            Ok(restored) => {
                restored_count += 1;

                let has_finalizer = restored
                    .metadata
                    .finalizers
                    .as_ref()
                    .is_some_and(|f| f.iter().any(|fi| fi == LEGACY_FINALIZER));

                if !has_finalizer {
                    missing_finalizers.push(format!("{}/{}", entry.namespace, entry.name));
                }

                let has_exists = {
                    let obj_value: Value = serde_json::to_value(&restored).map_err(|e| {
                        MigrationError::Serialization(
                            format!(
                                "serialize restored object for status check {}/{}",
                                entry.namespace, entry.name
                            ),
                            e,
                        )
                    })?;
                    obj_value
                        .get("status")
                        .and_then(|status| status.get("conditions"))
                        .and_then(|c| c.as_array())
                        .is_some_and(|conditions| {
                            conditions.iter().any(|c| {
                                c.get("type")
                                    .and_then(|t| t.as_str())
                                    .is_some_and(|t| t == "Exists")
                                    && c.get("status")
                                        .and_then(|s| s.as_str())
                                        .is_some_and(|s| s == "True")
                            })
                        })
                };

                if !has_exists {
                    missing_exists.push(format!("{}/{}", entry.namespace, entry.name));
                }

                let restored_value: Value = serde_json::to_value(&restored).map_err(|e| {
                    MigrationError::Serialization(
                        format!(
                            "serialize restored object {}/{}",
                            entry.namespace, entry.name
                        ),
                        e,
                    )
                })?;

                let backup_obj = get_backup_object(backup_api, &entry.secret_name).await?;
                let backup_value = backup_obj.ok_or_else(|| {
                    MigrationError::Backup(format!(
                        "backup secret {} missing object.json",
                        entry.secret_name
                    ))
                })?;

                if metadata_spec_checksum_match(&backup_value, &restored_value) {
                    checksum_matches += 1;
                } else {
                    checksum_mismatches.push(format!("{}/{}", entry.namespace, entry.name));
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                missing_restorations.push(format!("{}/{}", entry.namespace, entry.name));
            }
            Err(e) => {
                return Err(MigrationError::Kube(
                    format!("get restored object {}/{}", entry.namespace, entry.name),
                    Box::new(e),
                ));
            }
        }
    }

    Ok(AdoptionVerificationResult {
        corrected_crd_established: corrected_established,
        legacy_crd_absent: !legacy_present,
        source_count,
        restored_count,
        checksum_matches,
        checksum_mismatches,
        missing_restorations,
        missing_finalizers,
        missing_exists,
    })
}

pub fn adoption_verification_passed(result: &AdoptionVerificationResult) -> bool {
    result.corrected_crd_established
        && result.legacy_crd_absent
        && result.source_count == result.restored_count
        && result.checksum_matches == result.source_count
        && result.checksum_mismatches.is_empty()
        && result.missing_restorations.is_empty()
        && result.missing_finalizers.is_empty()
        && result.missing_exists.is_empty()
}

fn metadata_spec_checksum_match(backup: &Value, restored: &Value) -> bool {
    let backup_meta_spec = extract_metadata_spec(backup);
    let restored_meta_spec = extract_metadata_spec(restored);

    match (backup_meta_spec, restored_meta_spec) {
        (Some(b), Some(r)) => object_checksum(&b) == object_checksum(&r),
        _ => false,
    }
}

fn extract_metadata_spec(value: &Value) -> Option<Value> {
    let meta = value.get("metadata")?;
    let spec = value.get("spec")?;

    let mut result = serde_json::Map::new();

    if let Some(meta_obj) = meta.as_object() {
        let mut clean_meta = serde_json::Map::new();
        for key in [
            "name",
            "namespace",
            "labels",
            "annotations",
            "ownerReferences",
        ] {
            if let Some(v) = meta_obj.get(key) {
                clean_meta.insert(key.to_string(), v.clone());
            }
        }
        result.insert("metadata".to_string(), Value::Object(clean_meta));
    }

    result.insert("spec".to_string(), spec.clone());
    Some(Value::Object(result))
}

async fn get_backup_object(
    api: &Api<k8s_openapi::api::core::v1::Secret>,
    secret_name: &str,
) -> Result<Option<Value>> {
    match api.get(secret_name).await {
        Ok(secret) => {
            let obj_json = secret
                .string_data
                .as_ref()
                .and_then(|d| d.get("object.json"))
                .map(|s| s.as_str())
                .or_else(|| {
                    secret
                        .data
                        .as_ref()
                        .and_then(|d| d.get("object.json"))
                        .and_then(|v| std::str::from_utf8(&v.0).ok())
                });

            match obj_json {
                Some(json_str) => {
                    let value: Value = serde_json::from_str(json_str).map_err(|e| {
                        MigrationError::Serialization(
                            format!("parse backup object from {secret_name}"),
                            e,
                        )
                    })?;
                    Ok(Some(value))
                }
                None => Ok(None),
            }
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(MigrationError::Kube(
            format!("get backup secret {secret_name}"),
            Box::new(e),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_structural_verification_passed_all_good() {
        let result = StructuralVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
        };
        assert!(structural_verification_passed(&result));
    }

    #[test]
    fn test_structural_verification_failed_crd_not_established() {
        let result = StructuralVerificationResult {
            corrected_crd_established: false,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
        };
        assert!(!structural_verification_passed(&result));
    }

    #[test]
    fn test_structural_verification_failed_legacy_still_present() {
        let result = StructuralVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: false,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
        };
        assert!(!structural_verification_passed(&result));
    }

    #[test]
    fn test_structural_verification_failed_count_mismatch() {
        let result = StructuralVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 3,
            checksum_matches: 3,
            checksum_mismatches: vec![],
            missing_restorations: vec!["default/alice".to_string(), "default/bob".to_string()],
        };
        assert!(!structural_verification_passed(&result));
    }

    #[test]
    fn test_structural_verification_failed_checksum_mismatch() {
        let result = StructuralVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 4,
            checksum_mismatches: vec!["default/alice".to_string()],
            missing_restorations: vec![],
        };
        assert!(!structural_verification_passed(&result));
    }

    #[test]
    fn test_structural_passes_without_finalizer_or_exists() {
        let result = StructuralVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
        };
        assert!(structural_verification_passed(&result));
    }

    #[test]
    fn test_adoption_verification_passed_all_good() {
        let result = AdoptionVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec![],
        };
        assert!(adoption_verification_passed(&result));
    }

    #[test]
    fn test_adoption_verification_failed_missing_finalizer() {
        let result = AdoptionVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec!["default/alice".to_string()],
            missing_exists: vec![],
        };
        assert!(!adoption_verification_passed(&result));
    }

    #[test]
    fn test_adoption_verification_failed_missing_exists() {
        let result = AdoptionVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 5,
            restored_count: 5,
            checksum_matches: 5,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec!["default/alice".to_string()],
        };
        assert!(!adoption_verification_passed(&result));
    }

    #[test]
    fn test_adoption_verification_zero_persons() {
        let result = AdoptionVerificationResult {
            corrected_crd_established: true,
            legacy_crd_absent: true,
            source_count: 0,
            restored_count: 0,
            checksum_matches: 0,
            checksum_mismatches: vec![],
            missing_restorations: vec![],
            missing_finalizers: vec![],
            missing_exists: vec![],
        };
        assert!(adoption_verification_passed(&result));
    }

    #[test]
    fn test_metadata_spec_checksum_match_identical() {
        let backup = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "labels": {"app": "test"},
                "annotations": {"note": "value"}
            },
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let restored = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "uid": "new-uid",
                "resourceVersion": "999",
                "creationTimestamp": "2024-01-01T00:00:00Z",
                "labels": {"app": "test"},
                "annotations": {"note": "value"},
                "finalizers": ["kanidmpersonsaccounts.kaniop.rs/finalizer"]
            },
            "spec": {"kanidmRef": {"name": "kanidm"}},
            "status": {"ready": true}
        });

        assert!(metadata_spec_checksum_match(&backup, &restored));
    }

    #[test]
    fn test_metadata_spec_checksum_mismatch_spec_changed() {
        let backup = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}, "displayName": "Alice"}
        });

        let restored = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}, "displayName": "Bob"}
        });

        assert!(!metadata_spec_checksum_match(&backup, &restored));
    }

    #[test]
    fn test_metadata_spec_checksum_mismatch_labels_changed() {
        let backup = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default", "labels": {"app": "test"}},
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let restored = serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default", "labels": {"app": "changed"}},
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        assert!(!metadata_spec_checksum_match(&backup, &restored));
    }

    #[test]
    fn test_extract_metadata_spec_filters_server_fields() {
        let value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Test",
            "metadata": {
                "name": "foo",
                "namespace": "default",
                "uid": "should-be-excluded",
                "resourceVersion": "should-be-excluded",
                "creationTimestamp": "should-be-excluded",
                "managedFields": [{"manager": "kubectl"}],
                "labels": {"app": "test"}
            },
            "spec": {"field": "value"},
            "status": {"ready": true}
        });

        let extracted = extract_metadata_spec(&value).unwrap();
        let meta = extracted.get("metadata").unwrap();
        assert!(meta.get("uid").is_none());
        assert!(meta.get("resourceVersion").is_none());
        assert!(meta.get("creationTimestamp").is_none());
        assert!(meta.get("managedFields").is_none());
        assert!(meta.get("name").is_some());
        assert!(meta.get("labels").is_some());
        assert!(extracted.get("spec").is_some());
        assert!(extracted.get("status").is_none());
    }
}
