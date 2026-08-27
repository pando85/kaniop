use kaniop_backup::crd::{KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule};
use kaniop_operator::kanidm::restore::{
    BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION,
};

use kube::ResourceExt;

pub fn validate_repository_prefix_unique(
    repository: &KanidmBackupRepository,
    store: &kube::runtime::reflector::Store<KanidmBackupRepository>,
) -> Result<(), String> {
    let obj_namespace = repository
        .namespace()
        .unwrap_or_else(|| "default".to_string());
    let obj_bucket = &repository.spec.s3.bucket;
    let obj_prefix = &repository.spec.s3.prefix;

    for existing in store.state() {
        if existing.uid() == repository.uid() {
            continue;
        }
        let existing_namespace = existing
            .namespace()
            .unwrap_or_else(|| "default".to_string());
        if existing_namespace != obj_namespace {
            continue;
        }
        if existing.spec.s3.bucket == *obj_bucket && existing.spec.s3.prefix == *obj_prefix {
            return Err(format!(
                "KanidmBackupRepository {}/{} uses the same (bucket, prefix) ({}, {}) as {}/{}; a (bucket, prefix) pair must be owned by exactly one Repository",
                obj_namespace,
                repository.name_any(),
                obj_bucket,
                obj_prefix,
                existing_namespace,
                existing.name_any(),
            ));
        }
    }
    Ok(())
}

pub fn validate_repository_immutable_after_use(
    old: &KanidmBackupRepository,
    new: &KanidmBackupRepository,
) -> Result<(), String> {
    let has_backups = old
        .status
        .as_ref()
        .is_some_and(|s| s.observed_generation.is_some());

    if !has_backups {
        return Ok(());
    }

    if old.spec.s3.bucket != new.spec.s3.bucket {
        return Err("s3.bucket is immutable after repository has been used".to_string());
    }
    if old.spec.s3.prefix != new.spec.s3.prefix {
        return Err("s3.prefix is immutable after repository has been used".to_string());
    }
    if old.spec.s3.endpoint != new.spec.s3.endpoint {
        return Err("s3.endpoint is immutable after repository has been used".to_string());
    }

    Ok(())
}

pub fn validate_schedule_unique_kanidm_target(
    schedule: &KanidmBackupSchedule,
    store: &kube::runtime::reflector::Store<KanidmBackupSchedule>,
) -> Result<(), String> {
    let obj_namespace = schedule
        .namespace()
        .unwrap_or_else(|| "default".to_string());
    let obj_kanidm = &schedule.spec.kanidm_ref.name;

    for existing in store.state() {
        if existing.uid() == schedule.uid() {
            continue;
        }
        let existing_namespace = existing
            .namespace()
            .unwrap_or_else(|| "default".to_string());
        if existing_namespace != obj_namespace {
            continue;
        }
        if existing.spec.kanidm_ref.name == *obj_kanidm {
            return Err(format!(
                "only one KanidmBackupSchedule may target Kanidm '{}' in namespace '{}'; conflicting schedule: {}/{}",
                obj_kanidm,
                obj_namespace,
                existing_namespace,
                existing.name_any(),
            ));
        }
    }
    Ok(())
}

pub fn validate_schedule_immutable_after_discovery(
    old: &KanidmBackupSchedule,
    new: &KanidmBackupSchedule,
) -> Result<(), String> {
    let has_discovered = old
        .status
        .as_ref()
        .and_then(|s| s.last_discovered_backup_ref.as_ref())
        .is_some();

    if !has_discovered {
        return Ok(());
    }

    if old.spec.kanidm_ref != new.spec.kanidm_ref {
        return Err("kanidmRef is immutable after first backup has been discovered".to_string());
    }
    if old.spec.repository_ref != new.spec.repository_ref {
        return Err(
            "repositoryRef is immutable after first backup has been discovered".to_string(),
        );
    }
    if old.spec.schedule != new.spec.schedule {
        return Err("schedule is immutable after first backup has been discovered".to_string());
    }
    if old.spec.retention != new.spec.retention {
        return Err("retention is immutable after first backup has been discovered".to_string());
    }

    Ok(())
}

pub fn validate_backup_immutable_spec(
    old: &KanidmBackup,
    new: &KanidmBackup,
) -> Result<(), String> {
    if old.spec != new.spec {
        return Err(
            "KanidmBackup spec is immutable after creation; catalog entries are derived from remote manifests and must not be modified".to_string(),
        );
    }
    Ok(())
}

pub fn validate_break_glass_annotations(
    annotations: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let has_reason = annotations.contains_key(BREAK_GLASS_REASON_ANNOTATION);
    let has_approver = annotations.contains_key(BREAK_GLASS_APPROVED_BY_ANNOTATION);

    if has_reason && !has_approver {
        return Err(format!(
            "break-glass annotation '{}' is set but '{}' is missing; break-glass requires both a non-empty reason and an approver",
            BREAK_GLASS_REASON_ANNOTATION, BREAK_GLASS_APPROVED_BY_ANNOTATION,
        ));
    }
    if !has_reason && has_approver {
        return Err(format!(
            "break-glass annotation '{}' is set but '{}' is missing; break-glass requires both a non-empty reason and an approver",
            BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION,
        ));
    }

    if has_reason {
        let reason = annotations
            .get(BREAK_GLASS_REASON_ANNOTATION)
            .map(|s| s.as_str())
            .unwrap_or("");
        let approver = annotations
            .get(BREAK_GLASS_APPROVED_BY_ANNOTATION)
            .map(|s| s.as_str())
            .unwrap_or("");
        if reason.trim().is_empty() {
            return Err(format!(
                "break-glass annotation '{}' must be non-empty",
                BREAK_GLASS_REASON_ANNOTATION,
            ));
        }
        if approver.trim().is_empty() {
            return Err(format!(
                "break-glass annotation '{}' must be non-empty",
                BREAK_GLASS_APPROVED_BY_ANNOTATION,
            ));
        }
    }

    Ok(())
}

pub fn validate_auth_method_exactly_one(
    auth: &kaniop_backup::crd::AuthMethod,
    field_path: &str,
) -> Result<(), String> {
    let count = auth.workload_identity.is_some() as u8 + auth.secret_ref.is_some() as u8;
    if count != 1 {
        return Err(format!(
            "{field_path} must have exactly one of workloadIdentity or secretRef set (found {count})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_backup::crd::{
        AuthMethod, BackupKanidmRef, BackupRepositoryRef, KanidmBackupRepositoryStatus,
        KanidmBackupScheduleStatus, RepositoryAuthentication, S3Config, ScheduleKanidmRef,
        ScheduleRepositoryRef, SecretRef, WorkloadIdentity,
    };
    use std::collections::BTreeMap;

    #[test]
    fn break_glass_both_present_and_non_empty_passes() {
        let mut annotations = BTreeMap::new();
        annotations.insert(
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "corrupt volume".to_string(),
        );
        annotations.insert(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "oncall-admin".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_ok());
    }

    #[test]
    fn break_glass_reason_without_approver_fails() {
        let mut annotations = BTreeMap::new();
        annotations.insert(
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "corrupt volume".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_err());
    }

    #[test]
    fn break_glass_empty_reason_fails() {
        let mut annotations = BTreeMap::new();
        annotations.insert(BREAK_GLASS_REASON_ANNOTATION.to_string(), "".to_string());
        annotations.insert(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "oncall-admin".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_err());
    }

    #[test]
    fn no_break_glass_annotations_passes() {
        let annotations = BTreeMap::new();
        assert!(validate_break_glass_annotations(&annotations).is_ok());
    }

    #[test]
    fn break_glass_approver_without_reason_fails() {
        let mut annotations = BTreeMap::new();
        annotations.insert(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "oncall-admin".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_err());
    }

    #[test]
    fn break_glass_whitespace_only_reason_fails() {
        let mut annotations = BTreeMap::new();
        annotations.insert(BREAK_GLASS_REASON_ANNOTATION.to_string(), "   ".to_string());
        annotations.insert(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "oncall-admin".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_err());
    }

    #[test]
    fn break_glass_whitespace_only_approver_fails() {
        let mut annotations = BTreeMap::new();
        annotations.insert(
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "corrupt volume".to_string(),
        );
        annotations.insert(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "   ".to_string(),
        );
        assert!(validate_break_glass_annotations(&annotations).is_err());
    }

    #[test]
    fn backup_immutable_spec_same_spec_passes() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "corp-idm".to_string(),
                    uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
                manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
            },
            status: None,
        };
        let new = old.clone();
        assert!(validate_backup_immutable_spec(&old, &new).is_ok());
    }

    #[test]
    fn backup_immutable_spec_changed_spec_fails() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "corp-idm".to_string(),
                    uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
                manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
            },
            status: None,
        };
        let mut new = old.clone();
        new.spec.manifest_key = "v1/tenants/ns/clusters/k/backups/b/manifest-v2.json".to_string();
        assert!(validate_backup_immutable_spec(&old, &new).is_err());
    }

    #[test]
    fn backup_immutable_spec_changed_backup_id_fails() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "corp-idm".to_string(),
                    uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
                manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
            },
            status: None,
        };
        let mut new = old.clone();
        new.spec.backup_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string();
        assert!(validate_backup_immutable_spec(&old, &new).is_err());
    }

    fn test_repository() -> KanidmBackupRepository {
        KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some("test-repo".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "my-bucket".to_string(),
                    prefix: "prod".to_string(),
                    region: Some("eu-west-1".to_string()),
                    endpoint: Some("https://s3.eu-west-1.amazonaws.com".to_string()),
                    force_path_style: false,
                    ca_bundle_ref: None,
                    insecure: false,
                },
                authentication: RepositoryAuthentication {
                    writer: AuthMethod {
                        workload_identity: Some(WorkloadIdentity { audience: None }),
                        secret_ref: None,
                    },
                    reader: AuthMethod {
                        workload_identity: Some(WorkloadIdentity { audience: None }),
                        secret_ref: None,
                    },
                    deleter: AuthMethod {
                        workload_identity: Some(WorkloadIdentity { audience: None }),
                        secret_ref: None,
                    },
                },
                encryption: None,
                limits: None,
            },
            status: None,
        }
    }

    #[test]
    fn repository_immutable_after_use_no_status_passes() {
        let old = test_repository();
        let mut new = old.clone();
        new.spec.s3.bucket = "different-bucket".to_string();
        assert!(validate_repository_immutable_after_use(&old, &new).is_ok());
    }

    #[test]
    fn repository_immutable_after_use_with_status_fails_on_bucket_change() {
        let mut old = test_repository();
        old.status = Some(KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.s3.bucket = "different-bucket".to_string();
        assert!(validate_repository_immutable_after_use(&old, &new).is_err());
    }

    #[test]
    fn repository_immutable_after_use_with_status_fails_on_prefix_change() {
        let mut old = test_repository();
        old.status = Some(KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.s3.prefix = "different-prefix".to_string();
        assert!(validate_repository_immutable_after_use(&old, &new).is_err());
    }

    #[test]
    fn repository_immutable_after_use_with_status_fails_on_endpoint_change() {
        let mut old = test_repository();
        old.status = Some(KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.s3.endpoint = Some("https://other.endpoint.com".to_string());
        assert!(validate_repository_immutable_after_use(&old, &new).is_err());
    }

    #[test]
    fn repository_immutable_after_use_with_status_allows_other_changes() {
        let mut old = test_repository();
        old.status = Some(KanidmBackupRepositoryStatus {
            observed_generation: Some(1),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.s3.region = Some("us-east-1".to_string());
        assert!(validate_repository_immutable_after_use(&old, &new).is_ok());
    }

    fn test_schedule() -> KanidmBackupSchedule {
        KanidmBackupSchedule {
            metadata: kube::api::ObjectMeta {
                name: Some("test-schedule".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: "corp-idm".to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: "offsite".to_string(),
                },
                schedule: "0 * * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend: false,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
            status: None,
        }
    }

    #[test]
    fn schedule_immutable_after_discovery_no_status_passes() {
        let old = test_schedule();
        let mut new = old.clone();
        new.spec.schedule = "*/5 * * * *".to_string();
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_ok());
    }

    #[test]
    fn schedule_immutable_after_discovery_with_status_fails_on_kanidm_ref_change() {
        let mut old = test_schedule();
        old.status = Some(KanidmBackupScheduleStatus {
            last_discovered_backup_ref: Some("kb-1".to_string()),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.kanidm_ref.name = "other-idm".to_string();
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_err());
    }

    #[test]
    fn schedule_immutable_after_discovery_with_status_fails_on_repository_ref_change() {
        let mut old = test_schedule();
        old.status = Some(KanidmBackupScheduleStatus {
            last_discovered_backup_ref: Some("kb-1".to_string()),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.repository_ref.name = "other-repo".to_string();
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_err());
    }

    #[test]
    fn schedule_immutable_after_discovery_with_status_fails_on_schedule_change() {
        let mut old = test_schedule();
        old.status = Some(KanidmBackupScheduleStatus {
            last_discovered_backup_ref: Some("kb-1".to_string()),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.schedule = "*/5 * * * *".to_string();
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_err());
    }

    #[test]
    fn schedule_immutable_after_discovery_with_status_fails_on_retention_change() {
        let mut old = test_schedule();
        old.status = Some(KanidmBackupScheduleStatus {
            last_discovered_backup_ref: Some("kb-1".to_string()),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.retention = Some(kaniop_backup::crd::RetentionPolicySpec {
            keep_last: 10,
            daily: 7,
            weekly: 4,
            monthly: 12,
            min_age: "24h".to_string(),
        });
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_err());
    }

    #[test]
    fn schedule_immutable_after_discovery_with_status_allows_suspend_change() {
        let mut old = test_schedule();
        old.status = Some(KanidmBackupScheduleStatus {
            last_discovered_backup_ref: Some("kb-1".to_string()),
            ..Default::default()
        });
        let mut new = old.clone();
        new.spec.suspend = true;
        assert!(validate_schedule_immutable_after_discovery(&old, &new).is_ok());
    }

    #[test]
    fn auth_method_exactly_one_workload_identity_passes() {
        let auth = AuthMethod {
            workload_identity: Some(WorkloadIdentity { audience: None }),
            secret_ref: None,
        };
        assert!(validate_auth_method_exactly_one(&auth, "writer").is_ok());
    }

    #[test]
    fn auth_method_exactly_one_secret_ref_passes() {
        let auth = AuthMethod {
            workload_identity: None,
            secret_ref: Some(SecretRef {
                name: "my-secret".to_string(),
            }),
        };
        assert!(validate_auth_method_exactly_one(&auth, "writer").is_ok());
    }

    #[test]
    fn auth_method_exactly_one_none_fails() {
        let auth = AuthMethod {
            workload_identity: None,
            secret_ref: None,
        };
        assert!(validate_auth_method_exactly_one(&auth, "writer").is_err());
    }

    #[test]
    fn auth_method_exactly_one_both_fails() {
        let auth = AuthMethod {
            workload_identity: Some(WorkloadIdentity { audience: None }),
            secret_ref: Some(SecretRef {
                name: "my-secret".to_string(),
            }),
        };
        assert!(validate_auth_method_exactly_one(&auth, "writer").is_err());
    }
}
