use serde::{Deserialize, Serialize};

pub const MANIFEST_API_VERSION_V1: &str = "backup.kaniop.rs/v1alpha1";
pub const MANIFEST_KIND: &str = "KanidmBackupManifest";
const MAX_STRING_LEN: usize = 4096;
const MAX_KEY_LEN: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported manifest api version: {0}")]
    UnsupportedVersion(String),
    #[error("invalid manifest kind: {0}")]
    InvalidKind(String),
    #[error("field exceeds maximum length: {field}")]
    FieldTooLong { field: String },
    #[error("invalid backup id: {0}")]
    InvalidBackupId(String),
    #[error("invalid payload key: {0}")]
    InvalidPayloadKey(String),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KanidmBackupManifest {
    pub api_version: String,
    pub kind: String,
    pub backup_id: String,
    pub created_at: String,
    pub source: ManifestSource,
    pub backup: ManifestBackup,
    pub payload: ManifestPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<ManifestEncryption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<ManifestCompatibility>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSource {
    pub namespace_uid: String,
    pub kanidm_name: String,
    pub kanidm_uid: String,
    pub domain: String,
    pub kanidm_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestBackup {
    pub mode: String,
    pub consistency: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPayload {
    pub key: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEncryption {
    pub transport: String,
    pub at_rest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_side: Option<ClientSideEncryptionMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSideEncryptionMeta {
    pub algorithm: String,
    pub wrapped_dek: String,
    pub nonce_salt: String,
    pub chunk_size_bytes: u64,
    pub kek_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCompatibility {
    pub same_kanidm_version_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_manifest_reader: Option<String>,
}

pub const SUPPORTED_MANIFEST_VERSIONS: &[&str] = &[MANIFEST_API_VERSION_V1];

impl KanidmBackupManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !SUPPORTED_MANIFEST_VERSIONS.contains(&self.api_version.as_str()) {
            return Err(ManifestError::UnsupportedVersion(self.api_version.clone()));
        }
        if self.kind != MANIFEST_KIND {
            return Err(ManifestError::InvalidKind(self.kind.clone()));
        }
        self.validate_backup_id()?;
        self.validate_payload_key()?;
        self.validate_string_lengths()?;
        Ok(())
    }

    fn validate_backup_id(&self) -> Result<(), ManifestError> {
        if self.backup_id.is_empty() || self.backup_id.len() > MAX_STRING_LEN {
            return Err(ManifestError::InvalidBackupId(self.backup_id.clone()));
        }
        uuid::Uuid::parse_str(&self.backup_id)
            .map_err(|_| ManifestError::InvalidBackupId(self.backup_id.clone()))?;
        Ok(())
    }

    fn validate_payload_key(&self) -> Result<(), ManifestError> {
        if self.payload.key.is_empty() || self.payload.key.len() > MAX_KEY_LEN {
            return Err(ManifestError::InvalidPayloadKey(self.payload.key.clone()));
        }
        if self.payload.key.contains("..") {
            return Err(ManifestError::InvalidPayloadKey(
                "payload key contains path traversal".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_string_lengths(&self) -> Result<(), ManifestError> {
        let fields = [
            ("source.namespaceUid", &self.source.namespace_uid),
            ("source.kanidmName", &self.source.kanidm_name),
            ("source.kanidmUid", &self.source.kanidm_uid),
            ("source.domain", &self.source.domain),
            ("source.kanidmVersion", &self.source.kanidm_version),
            ("backup.mode", &self.backup.mode),
            ("backup.consistency", &self.backup.consistency),
            ("backup.reason", &self.backup.reason),
            ("payload.sha256", &self.payload.sha256),
        ];
        for (name, value) in fields {
            if value.len() > MAX_STRING_LEN {
                return Err(ManifestError::FieldTooLong {
                    field: name.to_string(),
                });
            }
        }
        Ok(())
    }
}

pub fn parse_manifest(json: &str) -> Result<KanidmBackupManifest, ManifestError> {
    let manifest: KanidmBackupManifest = serde_json::from_str(json)?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn is_manifest_version_supported(version: &str) -> bool {
    SUPPORTED_MANIFEST_VERSIONS.contains(&version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> KanidmBackupManifest {
        KanidmBackupManifest {
            api_version: MANIFEST_API_VERSION_V1.to_string(),
            kind: MANIFEST_KIND.to_string(),
            backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
            created_at: "2026-08-18T02:03:41Z".to_string(),
            source: ManifestSource {
                namespace_uid: "a81c".to_string(),
                kanidm_name: "corp-idm".to_string(),
                kanidm_uid: "9e630aed-3a61-4418-b711-e6030fb67b51".to_string(),
                domain: "idm.example.es".to_string(),
                kanidm_version: "1.10.4".to_string(),
                image_digest: Some("sha256:abc".to_string()),
            },
            backup: ManifestBackup {
                mode: "full".to_string(),
                consistency: "kanidm-offline".to_string(),
                reason: "restore-safety".to_string(),
            },
            payload: ManifestPayload {
                key: "v1/tenants/a81c/clusters/9e630aed/backups/019c7c76/payload/kanidm.backup.json.gz"
                    .to_string(),
                size_bytes: 18432791,
                sha256: "9c8e".to_string(),
            },
            encryption: Some(ManifestEncryption {
                transport: "tls".to_string(),
                at_rest: "provider-kms".to_string(),
                key_id: Some("alias/kaniop-backups".to_string()),
                client_side: None,
            }),
            compatibility: Some(ManifestCompatibility {
                same_kanidm_version_required: true,
                minimum_manifest_reader: Some("0.13.0".to_string()),
            }),
        }
    }

    #[test]
    fn valid_manifest_passes_validation() {
        let m = valid_manifest();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut m = valid_manifest();
        m.api_version = "backup.kaniop.rs/v2".to_string();
        assert!(matches!(
            m.validate(),
            Err(ManifestError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn invalid_kind_is_rejected() {
        let mut m = valid_manifest();
        m.kind = "Wrong".to_string();
        assert!(matches!(m.validate(), Err(ManifestError::InvalidKind(_))));
    }

    #[test]
    fn invalid_backup_id_is_rejected() {
        let mut m = valid_manifest();
        m.backup_id = "not-a-uuid".to_string();
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidBackupId(_))
        ));
    }

    #[test]
    fn empty_backup_id_is_rejected() {
        let mut m = valid_manifest();
        m.backup_id = String::new();
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidBackupId(_))
        ));
    }

    #[test]
    fn path_traversal_in_payload_key_is_rejected() {
        let mut m = valid_manifest();
        m.payload.key = "v1/tenants/../clusters/payload".to_string();
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidPayloadKey(_))
        ));
    }

    #[test]
    fn field_too_long_is_rejected() {
        let mut m = valid_manifest();
        m.source.domain = "x".repeat(MAX_STRING_LEN + 1);
        assert!(matches!(
            m.validate(),
            Err(ManifestError::FieldTooLong { .. })
        ));
    }

    #[test]
    fn roundtrip_serialization() {
        let m = valid_manifest();
        let json = serde_json::to_string(&m).unwrap();
        let parsed = parse_manifest(&json).unwrap();
        assert_eq!(m, parsed);
    }

    #[test]
    fn is_manifest_version_supported_works() {
        assert!(is_manifest_version_supported(MANIFEST_API_VERSION_V1));
        assert!(!is_manifest_version_supported("backup.kaniop.rs/v99"));
    }

    #[test]
    fn optional_fields_are_omitted_when_none() {
        let mut m = valid_manifest();
        m.encryption = None;
        m.compatibility = None;
        m.source.image_digest = None;
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("encryption"));
        assert!(!json.contains("compatibility"));
        assert!(!json.contains("imageDigest"));
    }

    #[test]
    fn parse_manifest_rejects_invalid_json() {
        let result = parse_manifest("not json");
        assert!(matches!(result, Err(ManifestError::Json(_))));
    }

    #[test]
    fn parse_manifest_rejects_empty_payload_key() {
        let mut m = valid_manifest();
        m.payload.key = String::new();
        let json = serde_json::to_string(&m).unwrap();
        let result = parse_manifest(&json);
        assert!(matches!(result, Err(ManifestError::InvalidPayloadKey(_))));
    }

    #[test]
    fn parse_manifest_rejects_payload_key_too_long() {
        let mut m = valid_manifest();
        m.payload.key = "x".repeat(MAX_KEY_LEN + 1);
        let json = serde_json::to_string(&m).unwrap();
        let result = parse_manifest(&json);
        assert!(matches!(result, Err(ManifestError::InvalidPayloadKey(_))));
    }

    #[test]
    fn parse_manifest_rejects_empty_backup_id() {
        let mut m = valid_manifest();
        m.backup_id = String::new();
        let json = serde_json::to_string(&m).unwrap();
        let result = parse_manifest(&json);
        assert!(matches!(result, Err(ManifestError::InvalidBackupId(_))));
    }

    #[test]
    fn manifest_without_encryption_and_compatibility_validates() {
        let mut m = valid_manifest();
        m.encryption = None;
        m.compatibility = None;
        assert!(m.validate().is_ok());
    }

    #[test]
    fn manifest_with_empty_kind_is_rejected() {
        let mut m = valid_manifest();
        m.kind = String::new();
        assert!(matches!(m.validate(), Err(ManifestError::InvalidKind(_))));
    }

    #[test]
    fn manifest_payload_key_with_single_dot_is_allowed() {
        let mut m = valid_manifest();
        m.payload.key = "v1/tenants/a/clusters/b/backups/c/payload/file.name".to_string();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn manifest_multiple_path_traversals_rejected() {
        let mut m = valid_manifest();
        m.payload.key = "v1/../../etc/passwd".to_string();
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidPayloadKey(_))
        ));
    }
}
