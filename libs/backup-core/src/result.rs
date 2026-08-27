use serde::{Deserialize, Serialize};

pub const RESULT_DOC_VERSION: &str = "backup.kaniop.rs/v1alpha1";
pub const MAX_RESULT_DOC_SIZE: usize = 16 * 1024;

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
}
