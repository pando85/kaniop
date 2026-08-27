use serde::{Deserialize, Serialize};

pub const OPERATION_DOC_VERSION: &str = "backup.kaniop.rs/v1alpha1";
pub const MAX_OPERATION_DOC_SIZE: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("unsupported operation document version: {0}")]
    UnsupportedVersion(String),
    #[error("invalid operation kind: {0}")]
    InvalidKind(String),
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("operation document exceeds maximum size of {MAX_OPERATION_DOC_SIZE} bytes")]
    DocumentTooLarge,
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationDocument {
    pub api_version: String,
    pub kind: String,
    #[serde(flatten)]
    pub spec: OperationSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum OperationSpec {
    Upload(UploadOperation),
    Download(DownloadOperation),
    Probe(ProbeOperation),
    DeletePlan(DeletePlanOperation),
    Discover(DiscoverOperation),
    Check(CheckOperation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOperation {
    pub path: String,
    pub result_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadOperation {
    pub payload_path: String,
    pub bucket: String,
    pub prefix: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<String>,
    #[serde(default)]
    pub insecure: bool,
    pub backup_id: String,
    pub namespace_uid: String,
    pub kanidm_uid: String,
    pub kanidm_name: String,
    pub domain: String,
    pub kanidm_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    pub consistency: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_key_id: Option<String>,
    pub result_path: String,
    #[serde(default = "default_max_concurrent_parts")]
    pub max_concurrent_parts: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadOperation {
    pub manifest_key: String,
    pub bucket: String,
    pub prefix: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<String>,
    #[serde(default)]
    pub insecure: bool,
    pub expected_backup_id: String,
    pub expected_kanidm_uid: String,
    pub expected_domain: String,
    pub output_path: String,
    pub result_path: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub manifest_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverOperation {
    pub bucket: String,
    pub prefix: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<String>,
    #[serde(default)]
    pub insecure: bool,
    pub namespace_uid: String,
    pub kanidm_uid: String,
    pub result_path: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeOperation {
    pub bucket: String,
    pub prefix: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<String>,
    #[serde(default)]
    pub insecure: bool,
    pub result_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePlanOperation {
    pub keys: Vec<String>,
    pub bucket: String,
    pub prefix: String,
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub force_path_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<String>,
    #[serde(default)]
    pub insecure: bool,
    pub result_path: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_max_concurrent_parts() -> u32 {
    4
}

fn default_max_retries() -> u32 {
    3
}

fn default_max_results() -> u32 {
    1000
}

impl OperationDocument {
    pub fn validate(&self) -> Result<(), OperationError> {
        if self.api_version != OPERATION_DOC_VERSION {
            return Err(OperationError::UnsupportedVersion(self.api_version.clone()));
        }
        if self.kind != "OperationDocument" {
            return Err(OperationError::InvalidKind(self.kind.clone()));
        }
        match &self.spec {
            OperationSpec::Upload(op) => {
                if op.bucket.is_empty() {
                    return Err(OperationError::MissingField("bucket".to_string()));
                }
                if op.backup_id.is_empty() {
                    return Err(OperationError::MissingField("backupId".to_string()));
                }
                if op.payload_path.is_empty() {
                    return Err(OperationError::MissingField("payloadPath".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
                if op.endpoint.is_empty() {
                    return Err(OperationError::MissingField("endpoint".to_string()));
                }
            }
            OperationSpec::Download(op) => {
                if op.bucket.is_empty() {
                    return Err(OperationError::MissingField("bucket".to_string()));
                }
                if op.manifest_key.is_empty() {
                    return Err(OperationError::MissingField("manifestKey".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
                if op.output_path.is_empty() {
                    return Err(OperationError::MissingField("outputPath".to_string()));
                }
                if op.endpoint.is_empty() {
                    return Err(OperationError::MissingField("endpoint".to_string()));
                }
            }
            OperationSpec::Probe(op) => {
                if op.bucket.is_empty() {
                    return Err(OperationError::MissingField("bucket".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
                if op.endpoint.is_empty() {
                    return Err(OperationError::MissingField("endpoint".to_string()));
                }
            }
            OperationSpec::DeletePlan(op) => {
                if op.bucket.is_empty() {
                    return Err(OperationError::MissingField("bucket".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
                if op.endpoint.is_empty() {
                    return Err(OperationError::MissingField("endpoint".to_string()));
                }
                if op.keys.is_empty() {
                    return Err(OperationError::MissingField("keys".to_string()));
                }
            }
            OperationSpec::Discover(op) => {
                if op.bucket.is_empty() {
                    return Err(OperationError::MissingField("bucket".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
                if op.endpoint.is_empty() {
                    return Err(OperationError::MissingField("endpoint".to_string()));
                }
                if op.namespace_uid.is_empty() {
                    return Err(OperationError::MissingField("namespaceUid".to_string()));
                }
                if op.kanidm_uid.is_empty() {
                    return Err(OperationError::MissingField("kanidmUid".to_string()));
                }
            }
            OperationSpec::Check(op) => {
                if op.path.is_empty() {
                    return Err(OperationError::MissingField("path".to_string()));
                }
                if op.result_path.is_empty() {
                    return Err(OperationError::MissingField("resultPath".to_string()));
                }
            }
        }
        Ok(())
    }
}

pub fn parse_operation_document(json: &str) -> Result<OperationDocument, OperationError> {
    if json.len() > MAX_OPERATION_DOC_SIZE {
        return Err(OperationError::DocumentTooLarge);
    }
    let doc: OperationDocument = serde_json::from_str(json)?;
    doc.validate()?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_upload_op() -> OperationDocument {
        OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Upload(UploadOperation {
                payload_path: "/staging/backup.json".to_string(),
                bucket: "test-bucket".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                namespace_uid: "ns-uid".to_string(),
                kanidm_uid: "k-uid".to_string(),
                kanidm_name: "corp-idm".to_string(),
                domain: "idm.example.com".to_string(),
                kanidm_version: "1.10.4".to_string(),
                image_digest: Some("sha256:abc".to_string()),
                consistency: "kanidm-offline".to_string(),
                reason: "restore-safety".to_string(),
                encryption_mode: Some("providerKms".to_string()),
                encryption_key_id: Some("alias/key".to_string()),
                result_path: "/result/result.json".to_string(),
                max_concurrent_parts: 4,
                max_retries: 3,
            }),
        }
    }

    #[test]
    fn valid_upload_operation_passes_validation() {
        let doc = valid_upload_op();
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut doc = valid_upload_op();
        doc.api_version = "wrong/v1".to_string();
        assert!(matches!(
            doc.validate(),
            Err(OperationError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn wrong_kind_is_rejected() {
        let mut doc = valid_upload_op();
        doc.kind = "Wrong".to_string();
        assert!(matches!(
            doc.validate(),
            Err(OperationError::InvalidKind(_))
        ));
    }

    #[test]
    fn empty_bucket_is_rejected() {
        let mut doc = valid_upload_op();
        if let OperationSpec::Upload(ref mut op) = doc.spec {
            op.bucket = String::new();
        }
        assert!(matches!(
            doc.validate(),
            Err(OperationError::MissingField(_))
        ));
    }

    #[test]
    fn upload_roundtrip_serialization() {
        let doc = valid_upload_op();
        let json = serde_json::to_string(&doc).unwrap();
        let parsed = parse_operation_document(&json).unwrap();
        assert_eq!(doc, parsed);
    }

    #[test]
    fn download_operation_validation() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Download(DownloadOperation {
                manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
                bucket: "b".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                expected_backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                expected_kanidm_uid: "k-uid".to_string(),
                expected_domain: "idm.example.com".to_string(),
                output_path: "/staging/payload".to_string(),
                result_path: "/result/result.json".to_string(),
                max_retries: 3,
                manifest_only: false,
            }),
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn discover_operation_validation() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Discover(DiscoverOperation {
                bucket: "b".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                namespace_uid: "ns-uid".to_string(),
                kanidm_uid: "k-uid".to_string(),
                result_path: "/result/result.json".to_string(),
                max_results: 1000,
                max_retries: 3,
            }),
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn discover_operation_empty_namespace_uid_rejected() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Discover(DiscoverOperation {
                bucket: "b".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                namespace_uid: String::new(),
                kanidm_uid: "k-uid".to_string(),
                result_path: "/result/result.json".to_string(),
                max_results: 1000,
                max_retries: 3,
            }),
        };
        assert!(matches!(
            doc.validate(),
            Err(OperationError::MissingField(_))
        ));
    }

    #[test]
    fn probe_operation_validation() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Probe(ProbeOperation {
                bucket: "b".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                result_path: "/result/result.json".to_string(),
            }),
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn delete_plan_operation_validation() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::DeletePlan(DeletePlanOperation {
                keys: vec!["key1".to_string(), "key2".to_string()],
                bucket: "b".to_string(),
                prefix: "prod".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                result_path: "/result/result.json".to_string(),
                max_retries: 3,
            }),
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn delete_plan_empty_keys_rejected() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::DeletePlan(DeletePlanOperation {
                keys: vec![],
                bucket: "b".to_string(),
                prefix: "p".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "r".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                result_path: "/r".to_string(),
                max_retries: 3,
            }),
        };
        assert!(matches!(
            doc.validate(),
            Err(OperationError::MissingField(_))
        ));
    }

    #[test]
    fn discover_operation_missing_uid_rejected() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Discover(DiscoverOperation {
                bucket: "b".to_string(),
                prefix: "p".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "r".to_string(),
                force_path_style: false,
                ca_bundle_path: None,
                insecure: false,
                namespace_uid: "".to_string(),
                kanidm_uid: "k".to_string(),
                result_path: "/r".to_string(),
                max_results: 100,
                max_retries: 3,
            }),
        };
        assert!(matches!(
            doc.validate(),
            Err(OperationError::MissingField(_))
        ));
    }

    #[test]
    fn check_operation_validation() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Check(CheckOperation {
                path: "/data/backup.json".to_string(),
                result_path: "/result/result.json".to_string(),
            }),
        };
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn check_operation_empty_path_rejected() {
        let doc = OperationDocument {
            api_version: OPERATION_DOC_VERSION.to_string(),
            kind: "OperationDocument".to_string(),
            spec: OperationSpec::Check(CheckOperation {
                path: String::new(),
                result_path: "/result/result.json".to_string(),
            }),
        };
        assert!(matches!(
            doc.validate(),
            Err(OperationError::MissingField(_))
        ));
    }
}
