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
            "message": "s3.bucket is immutable after creation. To use a different bucket, delete this KanidmBackupRepository and create a new one. Existing Backup CRs referencing this repository are not affected, but remote S3 data in the old bucket remains accessible only through the old Repository.",
            "rule": "self.s3.bucket == oldSelf.s3.bucket"
        },
        {
            "message": "s3.prefix is immutable after creation. To use a different prefix, delete this KanidmBackupRepository and create a new one. Existing Backup CRs referencing this repository are not affected, but remote S3 data under the old prefix remains accessible only through the old Repository.",
            "rule": "self.s3.prefix == oldSelf.s3.prefix"
        },
        {
            "message": "s3.endpoint is immutable after creation. To use a different endpoint, delete this KanidmBackupRepository and create a new one. Existing Backup CRs referencing this repository are not affected, but remote S3 data at the old endpoint remains accessible only through the old Repository.",
            "rule": "self.s3.endpoint == oldSelf.s3.endpoint"
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
    /// S3-compatible storage configuration. The bucket, prefix, and endpoint fields are immutable after the repository has been used.
    pub s3: S3Config,
    /// Authentication configuration for writer, reader, and deleter roles. These fields are mutable and can be updated to rotate credentials.
    pub authentication: RepositoryAuthentication,
    /// Encryption configuration for backup payloads. Supports provider-managed SSE (providerManaged),
    /// provider KMS SSE (providerKms), and client-side envelope encryption (clientSide). When absent, no encryption is applied.
    /// This field is mutable, but once a backup has been created with a given encryption configuration, the mode, keyId, and keyRef sub-fields become immutable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<RepositoryEncryption>,
    /// Transport limits and safety retention. This field is mutable.
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
            "rule": "self.endpoint.startsWith('https://') || self.insecure"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct S3Config {
    /// S3 bucket name. Must not be empty.
    ///
    /// This field is immutable after the repository has been used. To use a different bucket, delete this Repository and create a new one.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "bucket must not be empty", "rule": "self.size() > 0"}]))]
    pub bucket: String,
    /// Prefix within the bucket. Must not contain path traversal segments (..).
    ///
    /// This field is immutable after the repository has been used. To use a different prefix, delete this Repository and create a new one.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "prefix must not contain path traversal segments", "rule": "!self.contains('..')"}]))]
    pub prefix: String,
    /// AWS region. Required for all S3-compatible providers.
    pub region: String,
    /// S3 endpoint URL. Must use HTTPS unless insecure is enabled.
    ///
    /// This field is immutable after the repository has been used. To use a different endpoint, delete this Repository and create a new one.
    pub endpoint: String,
    /// Use path-style addressing (http://endpoint/bucket) instead of virtual-hosted-style (http://bucket.endpoint).
    #[serde(default)]
    pub force_path_style: bool,
    /// Reference to a ConfigMap or Secret containing a CA bundle for TLS verification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_ref: Option<String>,
    /// Allow HTTP endpoints. Not recommended for production use.
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
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "keyId is required when mode is providerKms and forbidden otherwise",
            "rule": "(self.mode == 'providerKms') == has(self.keyId)"
        },
        {
            "message": "keyRef is required when mode is clientSide and forbidden otherwise",
            "rule": "(self.mode == 'clientSide') == has(self.keyRef)"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryEncryption {
    pub mode: EncryptionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_ref: Option<SecretRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum EncryptionMode {
    #[serde(rename = "providerManaged")]
    ProviderManaged,
    #[serde(rename = "providerKms")]
    ProviderKms,
    #[serde(rename = "clientSide")]
    ClientSide,
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
            "message": "kanidmRef is immutable after creation. To target a different Kanidm, delete this KanidmBackupSchedule and create a new one. Existing Backup CRs and remote S3 data are not affected by Schedule deletion.",
            "rule": "self.kanidmRef == oldSelf.kanidmRef"
        },
        {
            "message": "repositoryRef is immutable after creation. To use a different repository, delete this KanidmBackupSchedule and create a new one. Existing Backup CRs and remote S3 data are not affected by Schedule deletion.",
            "rule": "self.repositoryRef == oldSelf.repositoryRef"
        },
        {
            "message": "schedule is immutable after creation. To change the cron schedule, delete this KanidmBackupSchedule and create a new one. Existing Backup CRs and remote S3 data are not affected by Schedule deletion.",
            "rule": "self.schedule == oldSelf.schedule"
        },
        {
            "message": "retention is immutable after creation. To change retention policy, delete this KanidmBackupSchedule and create a new one. Existing Backup CRs and remote S3 data are not affected by Schedule deletion; retention is applied at discovery time based on the current Schedule spec.",
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
    /// Reference to the Kanidm instance to back up. Only one KanidmBackupSchedule may target a given Kanidm at a time.
    ///
    /// This field is immutable after creation. To target a different Kanidm, delete this Schedule and create a new one.
    /// Existing Backup CRs and remote S3 data are not affected by Schedule deletion.
    pub kanidm_ref: ScheduleKanidmRef,
    /// Reference to the KanidmBackupRepository where backups will be stored.
    ///
    /// This field is immutable after creation. To use a different repository, delete this Schedule and create a new one.
    /// Existing Backup CRs and remote S3 data are not affected by Schedule deletion.
    pub repository_ref: ScheduleRepositoryRef,
    /// Cron schedule for Kanidm's online backup. Kaniop renders this into Kanidm's [online_backup] configuration.
    ///
    /// This field is immutable after creation. To change the schedule, delete this KanidmBackupSchedule and create a new one.
    /// Existing Backup CRs and remote S3 data are not affected by Schedule deletion.
    ///
    /// The online backup transport is experimental (see TransportExperimental condition). Kanidm has no documented
    /// completion contract; Kaniop does not report production backup success based on file stability heuristics alone.
    pub schedule: String,
    /// Time zone for the cron schedule. Defaults to UTC.
    #[serde(default = "default_timezone")]
    pub time_zone: String,
    /// Suspend the backup schedule. When true, Kaniop pauses the online backup configuration on the Kanidm primary.
    /// This field is mutable and can be changed at any time.
    #[serde(default)]
    pub suspend: bool,
    /// Concurrency policy. Must be 'Forbid'.
    #[serde(default = "default_concurrency_policy")]
    pub concurrency_policy: String,
    /// Random jitter in seconds added to the schedule to avoid thundering herd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_seconds: Option<u32>,
    /// Number of local backup versions to retain on the Kanidm PVC under /data/backups.
    #[serde(default = "default_local_versions")]
    pub local_versions: u32,
    /// Retention policy for remote backups in the repository. Applied at discovery time based on the current Schedule spec.
    ///
    /// This field is immutable after creation. To change retention policy, delete this Schedule and create a new one.
    /// Existing Backup CRs and remote S3 data are not affected by Schedule deletion; retention is applied at discovery
    /// time based on the current Schedule spec.
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
pub struct DiscoveryStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_successful_scan_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(extend("x-kubernetes-validations" = [{"message": "lastDiscoveredCount must be non-negative", "rule": "self >= 0"}])))]
    pub last_discovered_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryStatus>,
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
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".spec.kanidmRef.name"}"#,
    printcolumn = r#"{"name":"Backup Age","type":"date","jsonPath":".status.createdAt"}"#,
    printcolumn = r#"{"name":"Version","type":"string","jsonPath":".status.kanidmVersion"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Consistency","type":"string","jsonPath":".status.consistency"}"#
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
                region: "eu-west-1".to_string(),
                endpoint: "https://s3.eu-west-1.amazonaws.com".to_string(),
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
                key_ref: None,
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

    #[test]
    fn backup_printer_columns_show_age_and_version() {
        use kube::CustomResourceExt;

        let crd = serde_json::to_value(KanidmBackup::crd()).unwrap();
        let columns = &crd["spec"]["versions"][0]["additionalPrinterColumns"];
        let backup_age = columns
            .as_array()
            .unwrap()
            .iter()
            .find(|column| column["name"] == "Backup Age")
            .unwrap();
        let serialized = serde_json::to_string(columns).unwrap();

        assert_eq!(backup_age["type"], "date");
        assert_eq!(backup_age["jsonPath"], ".status.createdAt");
        assert!(serialized.contains("Version"));
        assert!(serialized.contains(".status.kanidmVersion"));
        assert!(!serialized.contains(".metadata.creationTimestamp"));
        assert!(!serialized.contains("BackupID"));
    }
}
