use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "s3.bucket is immutable",
            "rule": "self.s3.bucket == oldSelf.s3.bucket"
        },
        {
            "message": "s3.prefix is immutable",
            "rule": "self.s3.prefix == oldSelf.s3.prefix"
        },
        {
            "message": "s3.endpoint is immutable",
            "rule": "!has(oldSelf.s3.endpoint) || self.s3.endpoint == oldSelf.s3.endpoint"
        }
    ]))
)]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1alpha1",
    kind = "KanidmBackupRepository",
    plural = "kanidmbackuprepositories",
    singular = "kanidmbackuprepository",
    shortname = "kbr",
    namespaced,
    status = "KanidmBackupRepositoryStatus",
    printcolumn = r#"{"name":"Bucket","type":"string","jsonPath":".spec.s3.bucket"}"#,
    printcolumn = r#"{"name":"Prefix","type":"string","jsonPath":".spec.s3.prefix"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupRepositorySpec {
    pub s3: S3Config,
    pub authentication: RepositoryAuthentication,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<RepositoryEncryption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<RepositoryLimits>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "endpoint must use HTTPS unless insecure is enabled",
            "rule": "!has(self.endpoint) || self.endpoint.startsWith('https://') || self.insecure"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct S3Config {
    #[schemars(extend("x-kubernetes-validations" = [{"message": "bucket must not be empty", "rule": "self.size() > 0"}]))]
    pub bucket: String,
    #[schemars(extend("x-kubernetes-validations" = [{"message": "prefix must not contain path traversal segments", "rule": "!self.contains('..')"}]))]
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_ref: Option<String>,
    #[serde(default)]
    pub insecure: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RepositoryAuthentication {
    pub writer: AuthMethod,
    pub reader: AuthMethod,
    pub deleter: AuthMethod,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "exactly one of workloadIdentity or secretRef must be set",
            "rule": "(has(self.workloadIdentity) ? 1 : 0) + (has(self.secretRef) ? 1 : 0) == 1"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workload_identity: Option<WorkloadIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<SecretRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct WorkloadIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RepositoryEncryption {
    pub mode: EncryptionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum EncryptionMode {
    #[serde(rename = "providerManaged")]
    ProviderManaged,
    #[serde(rename = "providerKms")]
    ProviderKms,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "maxConcurrentParts must be between 1 and 64",
            "rule": "self.maxConcurrentParts >= 1 && self.maxConcurrentParts <= 64"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_upload_bytes_per_second: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_download_bytes_per_second: Option<u64>,
    #[serde(default = "default_max_concurrent_parts")]
    pub max_concurrent_parts: u32,
    #[serde(default = "default_safety_backup_retention")]
    pub safety_backup_min_retention: String,
}

fn default_max_concurrent_parts() -> u32 {
    4
}

fn default_safety_backup_retention() -> String {
    "720h".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupRepositoryStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "kanidmRef is immutable",
            "rule": "self.kanidmRef == oldSelf.kanidmRef"
        },
        {
            "message": "repositoryRef is immutable",
            "rule": "self.repositoryRef == oldSelf.repositoryRef"
        },
        {
            "message": "schedule is immutable",
            "rule": "self.schedule == oldSelf.schedule"
        },
        {
            "message": "retention is immutable",
            "rule": "!has(oldSelf.retention) || self.retention == oldSelf.retention"
        },
        {
            "message": "concurrencyPolicy must be Forbid",
            "rule": "self.concurrencyPolicy == 'Forbid'"
        },
        {
            "message": "localVersions must be between 1 and 3650",
            "rule": "self.localVersions >= 1 && self.localVersions <= 3650"
        }
    ]))
)]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1alpha1",
    kind = "KanidmBackupSchedule",
    plural = "kanidmbackupschedules",
    singular = "kanidmbackupschedule",
    shortname = "kbs",
    namespaced,
    status = "KanidmBackupScheduleStatus",
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".spec.kanidmRef.name"}"#,
    printcolumn = r#"{"name":"Schedule","type":"string","jsonPath":".spec.schedule"}"#,
    printcolumn = r#"{"name":"Suspended","type":"boolean","jsonPath":".spec.suspend"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupScheduleSpec {
    pub kanidm_ref: ScheduleKanidmRef,
    pub repository_ref: ScheduleRepositoryRef,
    pub schedule: String,
    #[serde(default = "default_timezone")]
    pub time_zone: String,
    #[serde(default)]
    pub suspend: bool,
    #[serde(default = "default_concurrency_policy")]
    pub concurrency_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_seconds: Option<u32>,
    #[serde(default = "default_local_versions")]
    pub local_versions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionPolicySpec>,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_concurrency_policy() -> String {
    "Forbid".to_string()
}

fn default_local_versions() -> u32 {
    7
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleKanidmRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRepositoryRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "keepLast must be between 1 and 3650",
            "rule": "self.keepLast >= 1 && self.keepLast <= 3650"
        },
        {
            "message": "daily must be between 0 and 3650",
            "rule": "self.daily >= 0 && self.daily <= 3650"
        },
        {
            "message": "weekly must be between 0 and 520",
            "rule": "self.weekly >= 0 && self.weekly <= 520"
        },
        {
            "message": "monthly must be between 0 and 120",
            "rule": "self.monthly >= 0 && self.monthly <= 120"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicySpec {
    #[serde(default = "default_keep_last")]
    pub keep_last: u32,
    #[serde(default = "default_daily")]
    pub daily: u32,
    #[serde(default = "default_weekly")]
    pub weekly: u32,
    #[serde(default = "default_monthly")]
    pub monthly: u32,
    #[serde(default = "default_min_age")]
    pub min_age: String,
}

fn default_keep_last() -> u32 {
    8
}

fn default_daily() -> u32 {
    7
}

fn default_weekly() -> u32 {
    4
}

fn default_monthly() -> u32 {
    12
}

fn default_min_age() -> String {
    "24h".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupScheduleStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_discovered_backup_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_successful_backup_time: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "KanidmBackup spec is immutable",
            "rule": "self == oldSelf"
        }
    ]))
)]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1alpha1",
    kind = "KanidmBackup",
    plural = "kanidmbackups",
    singular = "kanidmbackup",
    shortname = "kb",
    namespaced,
    status = "KanidmBackupStatus",
    printcolumn = r#"{"name":"BackupID","type":"string","jsonPath":".spec.backupId"}"#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".spec.kanidmRef.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Consistency","type":"string","jsonPath":".status.consistency"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupSpec {
    pub backup_id: String,
    pub kanidm_ref: BackupKanidmRef,
    pub repository_ref: BackupRepositoryRef,
    pub manifest_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BackupKanidmRef {
    pub name: String,
    pub uid: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BackupRepositoryRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum KanidmBackupPhase {
    #[default]
    Discovering,
    Ready,
    Deleting,
    Deleted,
    Invalid,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub phase: KanidmBackupPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_spec_serialization() {
        let spec = KanidmBackupRepositorySpec {
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
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("my-bucket"));
        assert!(json.contains("providerKms"));
    }

    #[test]
    fn schedule_spec_defaults() {
        let json = r#"{
            "kanidmRef": {"name": "corp-idm"},
            "repositoryRef": {"name": "offsite"},
            "schedule": "3 */6 * * *"
        }"#;
        let spec: KanidmBackupScheduleSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.time_zone, "UTC");
        assert_eq!(spec.concurrency_policy, "Forbid");
        assert_eq!(spec.local_versions, 7);
        assert!(!spec.suspend);
    }

    #[test]
    fn backup_phase_default() {
        let phase = KanidmBackupPhase::default();
        assert_eq!(phase, KanidmBackupPhase::Discovering);
    }

    #[test]
    fn backup_spec_is_immutable() {
        let spec = KanidmBackupSpec {
            backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
            kanidm_ref: BackupKanidmRef {
                name: "corp-idm".to_string(),
                uid: "9e630aed".to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: "offsite".to_string(),
            },
            manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: KanidmBackupSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, parsed);
    }

    #[test]
    fn retention_policy_spec_defaults() {
        let json = r#"{}"#;
        let spec: RetentionPolicySpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.keep_last, 8);
        assert_eq!(spec.daily, 7);
        assert_eq!(spec.weekly, 4);
        assert_eq!(spec.monthly, 12);
        assert_eq!(spec.min_age, "24h");
    }
}
