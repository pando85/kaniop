use kaniop_backup::crd::{
    AuthMethod, BackupKanidmRef, BackupRepositoryRef, EncryptionMode, KanidmBackup,
    KanidmBackupRepository, KanidmBackupRepositorySpec, KanidmBackupSchedule,
    KanidmBackupScheduleSpec, KanidmBackupSpec, RepositoryAuthentication, RepositoryEncryption,
    RepositoryLimits, RetentionPolicySpec, S3Config, ScheduleKanidmRef, ScheduleRepositoryRef,
    WorkloadIdentity,
};
use kube::api::ObjectMeta;

pub fn repository_example() -> KanidmBackupRepository {
    KanidmBackupRepository {
        metadata: ObjectMeta {
            name: Some("offsite".to_string()),
            namespace: Some("identity-prod".to_string()),
            ..Default::default()
        },
        spec: KanidmBackupRepositorySpec {
            s3: S3Config {
                bucket: "corp-kaniop-backups".to_string(),
                prefix: "prod".to_string(),
                region: Some("eu-west-1".to_string()),
                endpoint: Some("https://s3.eu-west-1.amazonaws.com".to_string()),
                force_path_style: false,
                insecure: false,
                ca_bundle_ref: None,
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
            encryption: Some(RepositoryEncryption {
                mode: EncryptionMode::ProviderKms,
                key_id: Some("alias/kaniop-backups".to_string()),
            }),
            limits: Some(RepositoryLimits {
                max_upload_bytes_per_second: Some(52428800),
                max_download_bytes_per_second: Some(104857600),
                max_concurrent_parts: 4,
                safety_backup_min_retention: "720h".to_string(),
            }),
        },
        status: None,
    }
}

pub fn schedule_example() -> KanidmBackupSchedule {
    KanidmBackupSchedule {
        metadata: ObjectMeta {
            name: Some("corp-idm-standard".to_string()),
            namespace: Some("identity-prod".to_string()),
            ..Default::default()
        },
        spec: KanidmBackupScheduleSpec {
            kanidm_ref: ScheduleKanidmRef {
                name: "corp-idm".to_string(),
            },
            repository_ref: ScheduleRepositoryRef {
                name: "offsite".to_string(),
            },
            schedule: "3 */6 * * *".to_string(),
            time_zone: "UTC".to_string(),
            suspend: false,
            concurrency_policy: "Forbid".to_string(),
            jitter_seconds: Some(300),
            local_versions: 7,
            retention: Some(RetentionPolicySpec {
                keep_last: 8,
                daily: 7,
                weekly: 4,
                monthly: 12,
                min_age: "24h".to_string(),
            }),
        },
        status: None,
    }
}

pub fn backup_example() -> KanidmBackup {
    KanidmBackup {
        metadata: ObjectMeta {
            name: Some("corp-idm-019c7c76".to_string()),
            namespace: Some("identity-prod".to_string()),
            ..Default::default()
        },
        spec: KanidmBackupSpec {
            backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
            kanidm_ref: BackupKanidmRef {
                name: "corp-idm".to_string(),
                uid: "9e630aed-3a61-4418-b711-e6030fb67b51".to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: "offsite".to_string(),
            },
            manifest_key: "v1/tenants/a81c/clusters/9e630aed/backups/019c7c76/manifest.json"
                .to_string(),
        },
        status: None,
    }
}
