use kaniop_backup_core::crd::{AuthMethod, KanidmBackupRepository, KanidmBackupSchedule};
use kube::ResourceExt;
use tracing::debug;

use crate::kanidm::crd::Kanidm;
use crate::kanidm::restore::RESTORE_ANNOTATION;

pub const TRANSPORT_SIDECAR_NAME: &str = "data-mover-transport";

#[derive(Debug, Clone)]
pub struct TransportSidecarConfig {
    pub operation_doc_json: String,
    pub auth_method: AuthMethod,
    pub ca_bundle_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub schedule: String,
    pub local_versions: u32,
    pub transport: Option<TransportSidecarConfig>,
}

pub fn backup_target_validation_error(kanidm: &Kanidm) -> Option<String> {
    let pvc_backed = kanidm.spec.storage.as_ref().is_some_and(|storage| {
        storage.volume_claim_template.is_some()
            && storage.empty_dir.is_none()
            && storage.ephemeral.is_none()
    });
    if !pvc_backed {
        return Some("backup requires PVC-backed storage".to_string());
    }

    let primary_count = kanidm
        .spec
        .replica_groups
        .iter()
        .filter(|group| group.primary_node)
        .count();
    if primary_count != 1 {
        return Some(format!(
            "backup requires exactly one primary replica group; found {primary_count}"
        ));
    }

    None
}

pub fn resolve_backup_config(
    kanidm: &Kanidm,
    schedules: &[KanidmBackupSchedule],
    repositories: &[KanidmBackupRepository],
) -> Option<BackupConfig> {
    let kanidm_name = kanidm.name_any();
    let matching_schedules: Vec<_> = schedules
        .iter()
        .filter(|schedule| schedule.spec.kanidm_ref.name == kanidm_name)
        .collect();

    if matching_schedules.len() != 1 {
        if matching_schedules.len() > 1 {
            debug!(
                kanidm = %kanidm_name,
                schedules = matching_schedules.len(),
                "multiple KanidmBackupSchedules target Kanidm; backup configuration is disabled"
            );
        }
        return None;
    }

    let schedule = matching_schedules[0];
    if schedule.spec.suspend || kanidm.annotations().contains_key(RESTORE_ANNOTATION) {
        return None;
    }

    if let Some(reason) = backup_target_validation_error(kanidm) {
        debug!(kanidm = %kanidm_name, %reason, "invalid Kanidm backup target");
        return None;
    }

    Some(BackupConfig {
        schedule: schedule.spec.schedule.clone(),
        local_versions: schedule.spec.local_versions,
        transport: resolve_transport_sidecar_config(kanidm, schedule, repositories),
    })
}

fn resolve_transport_sidecar_config(
    kanidm: &Kanidm,
    schedule: &KanidmBackupSchedule,
    repositories: &[KanidmBackupRepository],
) -> Option<TransportSidecarConfig> {
    let kanidm_name = kanidm.name_any();
    if kanidm.status.is_none() {
        debug!(
            kanidm = %kanidm_name,
            "Kanidm status not yet populated, deferring transport sidecar injection"
        );
        return None;
    }

    let kanidm_version = kanidm
        .status
        .as_ref()
        .and_then(|status| status.version.as_ref())
        .map(|version| version.image_tag.clone())
        .unwrap_or_default();
    if kanidm_version.is_empty() {
        debug!(
            kanidm = %kanidm_name,
            "Kanidm version not yet populated, deferring transport sidecar injection"
        );
        return None;
    }

    let repo_name = &schedule.spec.repository_ref.name;
    let repository = repositories
        .iter()
        .find(|repo| repo.name_any() == *repo_name)?;
    if !is_repository_ready(repository) {
        return None;
    }

    let operation_doc_json = build_transport_operation_doc(kanidm, repository)?;
    Some(TransportSidecarConfig {
        operation_doc_json,
        auth_method: repository.spec.authentication.writer.clone(),
        ca_bundle_ref: repository.spec.s3.ca_bundle_ref.clone(),
    })
}

fn is_repository_ready(repo: &KanidmBackupRepository) -> bool {
    repo.status
        .as_ref()
        .and_then(|status| {
            status
                .conditions
                .iter()
                .find(|condition| condition.type_ == "Ready")
                .map(|condition| condition.status == "True" && condition.reason == "Accepted")
        })
        .unwrap_or(false)
}

fn build_transport_operation_doc(
    kanidm: &Kanidm,
    repository: &KanidmBackupRepository,
) -> Option<String> {
    let namespace_uid = kanidm.namespace().unwrap_or_default();
    let kanidm_uid = kanidm.metadata.uid.clone().unwrap_or_default();
    let kanidm_name = kanidm.name_any();
    let domain = kanidm.spec.domain.clone();
    let kanidm_version = kanidm
        .status
        .as_ref()
        .and_then(|status| status.version.as_ref())
        .map(|version| version.image_tag.clone())
        .unwrap_or_default();

    let s3 = &repository.spec.s3;
    let encryption = repository.spec.encryption.as_ref();
    let doc = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "transport",
        "watchDir": "/data/backups",
        "filePrefix": "backup-",
        "fileSuffix": ".json.gz",
        "pollIntervalSecs": 60,
        "minFileAgeSecs": 120,
        "bucket": s3.bucket,
        "prefix": s3.prefix,
        "endpoint": s3.endpoint,
        "region": s3.region,
        "forcePathStyle": s3.force_path_style,
        "insecure": s3.insecure,
        "caBundlePath": s3.ca_bundle_ref.as_ref().map(|_| "/etc/ssl/certs/ca-certificates.crt"),
        "namespaceUid": namespace_uid,
        "kanidmUid": kanidm_uid,
        "kanidmName": kanidm_name,
        "domain": domain,
        "kanidmVersion": kanidm_version,
        "imageDigest": null,
        "consistency": "kanidm-online",
        "reason": "scheduled",
        "encryptionMode": encryption.map(|value| serde_json::json!(value.mode)),
        "encryptionKeyId": encryption.and_then(|value| value.key_id.clone()),
        "maxConcurrentParts": 4,
        "maxRetries": 3
    });

    serde_json::to_string(&doc).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_backup_core::crd::{
        AuthMethod, KanidmBackupRepositorySpec, KanidmBackupScheduleSpec, RepositoryAuthentication,
        S3Config, ScheduleKanidmRef, ScheduleRepositoryRef, SecretRef,
    };
    use kube::api::ObjectMeta;

    use crate::kanidm::crd::{
        KanidmSpec, KanidmStorage, PersistentVolumeClaimTemplate, ReplicaGroup,
    };

    fn make_kanidm_with_status(name: &str, namespace: &str, version_tag: Option<&str>) -> Kanidm {
        let status = version_tag.map(|tag| crate::kanidm::crd::KanidmStatus {
            version: Some(crate::kanidm::crd::KanidmVersionStatus {
                image_tag: tag.to_string(),
                upgrade_check_result: crate::kanidm::crd::KanidmUpgradeCheckResult::Passed,
                compatibility_result: crate::kanidm::crd::VersionCompatibilityResult::Compatible,
            }),
            ..Default::default()
        });
        Kanidm {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                uid: Some("kanidm-uid-123".to_string()),
                ..Default::default()
            },
            spec: KanidmSpec {
                domain: "idm.example.com".to_string(),
                storage: Some(KanidmStorage {
                    volume_claim_template: Some(PersistentVolumeClaimTemplate::default()),
                    ..Default::default()
                }),
                replica_groups: vec![ReplicaGroup {
                    name: "default".to_string(),
                    replicas: 1,
                    primary_node: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            status,
        }
    }

    fn make_schedule(
        name: &str,
        kanidm_name: &str,
        repo_name: &str,
        suspend: bool,
    ) -> KanidmBackupSchedule {
        KanidmBackupSchedule {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupScheduleSpec {
                kanidm_ref: ScheduleKanidmRef {
                    name: kanidm_name.to_string(),
                },
                repository_ref: ScheduleRepositoryRef {
                    name: repo_name.to_string(),
                },
                schedule: "0 2 * * *".to_string(),
                time_zone: "UTC".to_string(),
                suspend,
                concurrency_policy: "Forbid".to_string(),
                jitter_seconds: None,
                local_versions: 7,
                retention: None,
            },
            status: None,
        }
    }

    fn make_repository(name: &str, ready: bool) -> KanidmBackupRepository {
        let method = AuthMethod {
            workload_identity: None,
            secret_ref: Some(SecretRef {
                name: "backup-secret".to_string(),
            }),
        };
        let mut repo = KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmBackupRepositorySpec {
                s3: S3Config {
                    bucket: "test-bucket".to_string(),
                    prefix: "backups".to_string(),
                    region: Some("us-east-1".to_string()),
                    endpoint: Some("https://s3.example.com".to_string()),
                    force_path_style: false,
                    ca_bundle_ref: None,
                    insecure: false,
                },
                authentication: RepositoryAuthentication {
                    writer: method.clone(),
                    reader: method.clone(),
                    deleter: method,
                },
                encryption: None,
                limits: None,
            },
            status: None,
        };
        if ready {
            repo.status = Some(kaniop_backup_core::crd::KanidmBackupRepositoryStatus {
                observed_generation: Some(1),
                conditions: vec![k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
                    type_: "Ready".to_string(),
                    status: "True".to_string(),
                    observed_generation: Some(1),
                    last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                        k8s_openapi::jiff::Timestamp::new(1704067200, 0).unwrap(),
                    ),
                    reason: "Accepted".to_string(),
                    message: "Repository configuration accepted".to_string(),
                }],
            });
        }
        repo
    }

    #[test]
    fn no_schedule_means_no_backup_config() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", None);
        assert!(resolve_backup_config(&kanidm, &[], &[]).is_none());
    }

    #[test]
    fn suspended_schedule_disables_native_backup_and_transport() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", Some("1.0.0"));
        let schedule = make_schedule("sched", "test-kanidm", "repo", true);
        let repo = make_repository("repo", true);
        assert!(resolve_backup_config(&kanidm, &[schedule], &[repo]).is_none());
    }

    #[test]
    fn schedule_drives_native_backup_even_when_repository_is_not_ready() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", Some("1.0.0"));
        let schedule = make_schedule("sched", "test-kanidm", "repo", false);
        let repo = make_repository("repo", false);
        let config = resolve_backup_config(&kanidm, &[schedule], &[repo]).unwrap();
        assert_eq!(config.schedule, "0 2 * * *");
        assert_eq!(config.local_versions, 7);
        assert!(config.transport.is_none());
    }

    #[test]
    fn native_backup_does_not_wait_for_kanidm_status() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", None);
        let schedule = make_schedule("sched", "test-kanidm", "repo", false);
        let repo = make_repository("repo", true);
        let config = resolve_backup_config(&kanidm, &[schedule], &[repo]).unwrap();
        assert!(config.transport.is_none());
    }

    #[test]
    fn ready_repository_adds_transport() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", Some("1.0.0"));
        let schedule = make_schedule("sched", "test-kanidm", "repo", false);
        let repo = make_repository("repo", true);
        let config = resolve_backup_config(&kanidm, &[schedule], &[repo]).unwrap();
        let transport = config.transport.expect("transport should be configured");
        assert!(transport.operation_doc_json.contains("transport"));
        assert!(transport.operation_doc_json.contains("test-bucket"));
    }

    #[test]
    fn multiple_schedules_fail_closed_even_if_one_is_suspended() {
        let kanidm = make_kanidm_with_status("test-kanidm", "default", Some("1.0.0"));
        let schedule1 = make_schedule("sched1", "test-kanidm", "repo1", false);
        let schedule2 = make_schedule("sched2", "test-kanidm", "repo2", true);
        let repo1 = make_repository("repo1", true);
        let repo2 = make_repository("repo2", true);
        assert!(resolve_backup_config(&kanidm, &[schedule1, schedule2], &[repo1, repo2]).is_none());
    }

    #[test]
    fn invalid_backup_target_fails_closed() {
        let mut kanidm = make_kanidm_with_status("test-kanidm", "default", Some("1.0.0"));
        kanidm.spec.storage = None;
        let schedule = make_schedule("sched", "test-kanidm", "repo", false);
        let repo = make_repository("repo", true);
        assert!(resolve_backup_config(&kanidm, &[schedule], &[repo]).is_none());
    }
}
