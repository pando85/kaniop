use serde::{Deserialize, Serialize};

pub const RESULT_DOC_VERSION: &str = "backup.kaniop.rs/v1alpha1";
pub const MAX_RESULT_DOC_SIZE: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ResultDocError {
    #[error("result document exceeds maximum size of {MAX_RESULT_DOC_SIZE} bytes")]
    DocumentTooLarge,
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitCode {
    Success = 0,
    Retryable = 1,
    InvalidInput = 2,
    Integrity = 3,
    Authorization = 4,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultDocument {
    pub api_version: String,
    pub kind: String,
    pub operation: String,
    pub success: bool,
    pub exit_code: ExitCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResultError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletion: Option<DeletionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoverResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionResult {
    pub deleted_keys: Vec<String>,
    pub failed_keys: Vec<FailedKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedKey {
    pub key: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverResult {
    pub manifest_keys: Vec<String>,
    pub total_found: u32,
    pub truncated: bool,
}

impl ResultDocument {
    pub fn success(operation: &str) -> Self {
        Self {
            api_version: RESULT_DOC_VERSION.to_string(),
            kind: "ResultDocument".to_string(),
            operation: operation.to_string(),
            success: true,
            exit_code: ExitCode::Success,
            backup_id: None,
            manifest_key: None,
            payload_key: None,
            payload_sha256: None,
            payload_size_bytes: None,
            created_at: None,
            consistency: None,
            reason: None,
            kanidm_version: None,
            image_digest: None,
            error: None,
            deletion: None,
            discovery: None,
        }
    }

    pub fn failure(operation: &str, exit_code: ExitCode, code: &str, message: &str) -> Self {
        Self {
            api_version: RESULT_DOC_VERSION.to_string(),
            kind: "ResultDocument".to_string(),
            operation: operation.to_string(),
            success: false,
            exit_code,
            backup_id: None,
            manifest_key: None,
            payload_key: None,
            payload_sha256: None,
            payload_size_bytes: None,
            created_at: None,
            consistency: None,
            reason: None,
            kanidm_version: None,
            image_digest: None,
            error: Some(ResultError {
                code: code.to_string(),
                message: message.to_string(),
            }),
            deletion: None,
            discovery: None,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

pub fn parse_result_document(json: &str) -> Result<ResultDocument, ResultDocError> {
    if json.len() > MAX_RESULT_DOC_SIZE {
        return Err(ResultDocError::DocumentTooLarge);
    }
    Ok(serde_json::from_str(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_result_serialization() {
        let mut result = ResultDocument::success("upload");
        result.backup_id = Some("019c7c76-f423-7a12-8f41-2bea7588a303".to_string());
        result.manifest_key = Some("v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string());
        result.payload_key = Some("v1/tenants/ns/clusters/k/backups/b/payload/data".to_string());
        result.payload_sha256 = Some("abc123".to_string());
        result.payload_size_bytes = Some(1024);
        let json = result.to_json().unwrap();
        assert!(json.contains("\"success\": true"));
        assert!(json.contains("019c7c76"));
        let parsed = parse_result_document(&json).unwrap();
        assert_eq!(result, parsed);
    }

    #[test]
    fn failure_result_serialization() {
        let result = ResultDocument::failure(
            "download",
            ExitCode::Integrity,
            "CHECKSUM_MISMATCH",
            "expected abc, got def",
        );
        let json = result.to_json().unwrap();
        assert!(json.contains("\"success\": false"));
        assert!(json.contains("CHECKSUM_MISMATCH"));
        assert_eq!(result.exit_code, ExitCode::Integrity);
    }

    #[test]
    fn exit_codes_are_correct() {
        assert_eq!(ExitCode::Success.as_i32(), 0);
        assert_eq!(ExitCode::Retryable.as_i32(), 1);
        assert_eq!(ExitCode::InvalidInput.as_i32(), 2);
        assert_eq!(ExitCode::Integrity.as_i32(), 3);
        assert_eq!(ExitCode::Authorization.as_i32(), 4);
    }

    #[test]
    fn deletion_result_roundtrip() {
        let mut result = ResultDocument::success("delete-plan");
        result.deletion = Some(DeletionResult {
            deleted_keys: vec!["key1".to_string()],
            failed_keys: vec![FailedKey {
                key: "key2".to_string(),
                reason: "ObjectLock".to_string(),
            }],
        });
        let json = result.to_json().unwrap();
        let parsed = parse_result_document(&json).unwrap();
        assert_eq!(result, parsed);
    }

    #[test]
    fn optional_fields_are_omitted_when_none() {
        let result = ResultDocument::success("upload");
        let json = result.to_json().unwrap();
        assert!(!json.contains("backupId"));
        assert!(!json.contains("manifestKey"));
        assert!(!json.contains("reason"));
        assert!(!json.contains("kanidmVersion"));
        assert!(!json.contains("imageDigest"));
        assert!(!json.contains("error"));
        assert!(!json.contains("discovery"));
    }

    #[test]
    fn discovery_result_roundtrip() {
        let mut result = ResultDocument::success("discover");
        result.discovery = Some(DiscoverResult {
            manifest_keys: vec![
                "prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json".to_string(),
                "prod/v1/tenants/ns/clusters/k/backups/b2/manifest.json".to_string(),
            ],
            total_found: 2,
            truncated: false,
        });
        let json = result.to_json().unwrap();
        let parsed = parse_result_document(&json).unwrap();
        assert_eq!(result, parsed);
        assert_eq!(parsed.discovery.unwrap().total_found, 2);
    }

    #[test]
    fn parse_at_max_size_succeeds() {
        let mut doc = ResultDocument::success("discover");
        let mut keys = Vec::new();
        let mut total_len = 0usize;
        let target = MAX_RESULT_DOC_SIZE - 512;
        for i in 0.. {
            let key = format!(
                "prod/v1/tenants/default-ns/clusters/kaniop/backups/{:032x}-f423-7a12-8f41-2bea7588a303/manifest.json",
                i
            );
            let entry_len = key.len() + 3;
            if total_len + entry_len > target {
                break;
            }
            total_len += entry_len;
            keys.push(key);
        }
        doc.discovery = Some(DiscoverResult {
            manifest_keys: keys,
            total_found: 0,
            truncated: false,
        });
        if let Some(ref mut d) = doc.discovery {
            d.total_found = d.manifest_keys.len() as u32;
        }
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            json.len() <= MAX_RESULT_DOC_SIZE,
            "serialized doc len {} exceeds MAX_RESULT_DOC_SIZE {}",
            json.len(),
            MAX_RESULT_DOC_SIZE
        );
        assert!(json.len() > MAX_RESULT_DOC_SIZE / 2);
        assert!(parse_result_document(&json).is_ok());
    }

    #[test]
    fn parse_over_max_size_fails() {
        let huge = "x".repeat(MAX_RESULT_DOC_SIZE + 1);
        let err = parse_result_document(&huge);
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ResultDocError::DocumentTooLarge));
    }

    #[test]
    fn parse_max_results_keys_within_limit() {
        let keys: Vec<String> = (0..1000)
            .map(|i| {
                format!(
                    "prod/v1/tenants/default-ns/clusters/kaniop/backups/{:032x}-f423-7a12-8f41-2bea7588a303/manifest.json",
                    i
                )
            })
            .collect();
        let doc = ResultDocument {
            api_version: RESULT_DOC_VERSION.to_string(),
            kind: "ResultDocument".to_string(),
            operation: "discover".to_string(),
            success: true,
            exit_code: ExitCode::Success,
            backup_id: None,
            manifest_key: None,
            payload_key: None,
            payload_sha256: None,
            payload_size_bytes: None,
            created_at: None,
            consistency: None,
            reason: None,
            kanidm_version: None,
            image_digest: None,
            error: None,
            deletion: None,
            discovery: Some(DiscoverResult {
                manifest_keys: keys,
                total_found: 1000,
                truncated: true,
            }),
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            json.len() <= MAX_RESULT_DOC_SIZE,
            "1000 manifest keys serialized to {} bytes, exceeds MAX_RESULT_DOC_SIZE {MAX_RESULT_DOC_SIZE}",
            json.len()
        );
        assert!(parse_result_document(&json).is_ok());
    }

    #[test]
    fn parse_1000_manifest_keys_size_gt_4096() {
        let keys: Vec<String> = (0..1000)
            .map(|i| {
                format!(
                    "prod/v1/tenants/default-ns/clusters/kaniop/backups/{:032x}-f423-7a12-8f41-2bea7588a303/manifest.json",
                    i
                )
            })
            .collect();
        let doc = ResultDocument {
            api_version: RESULT_DOC_VERSION.to_string(),
            kind: "ResultDocument".to_string(),
            operation: "discover".to_string(),
            success: true,
            exit_code: ExitCode::Success,
            backup_id: None,
            manifest_key: None,
            payload_key: None,
            payload_sha256: None,
            payload_size_bytes: None,
            created_at: None,
            consistency: None,
            reason: None,
            kanidm_version: None,
            image_digest: None,
            error: None,
            deletion: None,
            discovery: Some(DiscoverResult {
                manifest_keys: keys,
                total_found: 1000,
                truncated: true,
            }),
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            json.len() > 4096,
            "1000 manifest keys serialized to {} bytes, expected >4096",
            json.len()
        );
        let parsed = parse_result_document(&json).unwrap();
        assert_eq!(parsed.discovery.unwrap().manifest_keys.len(), 1000);
    }

    #[test]
    fn manifest_metadata_roundtrip() {
        let mut result = ResultDocument::success("download");
        result.backup_id = Some("019c7c76-f423-7a12-8f41-2bea7588a303".to_string());
        result.manifest_key = Some("v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string());
        result.payload_key = Some("v1/tenants/ns/clusters/k/backups/b/payload/data".to_string());
        result.payload_sha256 = Some("abc123".to_string());
        result.payload_size_bytes = Some(1024);
        result.created_at = Some("2026-08-18T02:03:41Z".to_string());
        result.consistency = Some("kanidm-offline".to_string());
        result.reason = Some("restore-safety".to_string());
        result.kanidm_version = Some("1.10.4".to_string());
        result.image_digest = Some("sha256:abc".to_string());
        let json = result.to_json().unwrap();
        let parsed = parse_result_document(&json).unwrap();
        assert_eq!(parsed.created_at.as_deref(), Some("2026-08-18T02:03:41Z"));
        assert_eq!(parsed.consistency.as_deref(), Some("kanidm-offline"));
        assert_eq!(parsed.reason.as_deref(), Some("restore-safety"));
        assert_eq!(parsed.kanidm_version.as_deref(), Some("1.10.4"));
        assert_eq!(parsed.image_digest.as_deref(), Some("sha256:abc"));
        assert_eq!(result, parsed);
    }

    #[test]
    fn manifest_metadata_omitted_when_none() {
        let result = ResultDocument::success("download");
        let json = result.to_json().unwrap();
        assert!(!json.contains("createdAt"));
        assert!(!json.contains("consistency"));
        assert!(!json.contains("reason"));
        assert!(!json.contains("kanidmVersion"));
        assert!(!json.contains("imageDigest"));
    }

    #[test]
    fn backward_compatible_parse_without_manifest_metadata() {
        let json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "download",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "key",
            "payloadKey": "pk",
            "payloadSha256": "sha",
            "payloadSizeBytes": 100
        }"#;
        let parsed = parse_result_document(json).unwrap();
        assert!(parsed.created_at.is_none());
        assert!(parsed.consistency.is_none());
        assert!(parsed.reason.is_none());
        assert!(parsed.kanidm_version.is_none());
        assert!(parsed.image_digest.is_none());
        assert_eq!(
            parsed.backup_id.as_deref(),
            Some("019c7c76-f423-7a12-8f41-2bea7588a303")
        );
    }
}
