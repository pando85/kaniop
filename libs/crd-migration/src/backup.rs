use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use serde_json::Value;

use crate::checksum::{backup_secret_name, object_checksum};
use crate::{
    BACKUP_PREFIX, MIGRATION_LABEL, MIGRATION_SOURCE_ANNOTATION, MIGRATION_SOURCE_NS_HASH_LABEL,
    MIGRATION_VERSION, MigrationError, Result,
};

pub const MAX_BACKUP_SECRET_SIZE: usize = 900 * 1024;

pub struct BackupEntry {
    pub namespace: String,
    pub name: String,
    pub checksum: String,
    pub restored: bool,
    pub secret_name: String,
}

pub fn validate_backup_size(sanitized_obj: &Value, ns: &str, name: &str) -> Result<()> {
    let serialized = serde_json::to_vec(sanitized_obj)
        .map_err(|e| MigrationError::Serialization(format!("serialize backup {ns}/{name}"), e))?;

    if serialized.len() > MAX_BACKUP_SECRET_SIZE {
        return Err(MigrationError::Backup(format!(
            "backup for {ns}/{name} exceeds size limit: {} bytes > {} bytes limit",
            serialized.len(),
            MAX_BACKUP_SECRET_SIZE
        )));
    }

    Ok(())
}

pub fn create_backup_secret(
    sanitized_obj: &Value,
    source_ns: &str,
    source_name: &str,
    checksum: &str,
    backup_namespace: &str,
) -> Secret {
    let secret_name = backup_secret_name(BACKUP_PREFIX, source_ns, source_name);
    let source_ref = format!("{source_ns}/{source_name}");
    let ns_hash = crate::checksum::sha256_hex(source_ns.as_bytes());

    let mut data = BTreeMap::new();
    data.insert(
        "object.json".to_string(),
        serde_json::to_string_pretty(sanitized_obj).unwrap_or_default(),
    );
    data.insert("checksum".to_string(), checksum.to_string());
    data.insert("restored".to_string(), "false".to_string());

    let mut labels = BTreeMap::new();
    labels.insert(MIGRATION_LABEL.to_string(), MIGRATION_VERSION.to_string());
    labels.insert(
        MIGRATION_SOURCE_NS_HASH_LABEL.to_string(),
        ns_hash[..20].to_string(),
    );

    let mut annotations = BTreeMap::new();
    annotations.insert(MIGRATION_SOURCE_ANNOTATION.to_string(), source_ref);

    Secret {
        metadata: ObjectMeta {
            name: Some(secret_name.clone()),
            namespace: Some(backup_namespace.to_string()),
            labels: Some(labels),
            annotations: Some(annotations),
            ..Default::default()
        },
        string_data: Some(data),
        ..Default::default()
    }
}

pub async fn create_or_validate_backup(
    api: &Api<Secret>,
    sanitized_obj: &Value,
    source_ns: &str,
    source_name: &str,
    backup_namespace: &str,
) -> Result<BackupEntry> {
    validate_backup_size(sanitized_obj, source_ns, source_name)?;

    let checksum = object_checksum(sanitized_obj);
    let secret_name = backup_secret_name(BACKUP_PREFIX, source_ns, source_name);

    match api.get(&secret_name).await {
        Ok(existing) => {
            let existing_checksum = existing
                .string_data
                .as_ref()
                .and_then(|d| d.get("checksum"))
                .cloned()
                .or_else(|| {
                    existing
                        .data
                        .as_ref()
                        .and_then(|d| d.get("checksum"))
                        .map(|v| String::from_utf8_lossy(&v.0).to_string())
                })
                .ok_or_else(|| {
                    MigrationError::Backup(format!("backup secret {secret_name} missing checksum"))
                })?;

            if existing_checksum != checksum {
                return Err(MigrationError::ChecksumMismatch {
                    ns: source_ns.to_string(),
                    name: source_name.to_string(),
                    expected: existing_checksum,
                    actual: checksum,
                });
            }

            let restored = existing
                .string_data
                .as_ref()
                .and_then(|d| d.get("restored"))
                .map(|v| v == "true")
                .unwrap_or(false);

            Ok(BackupEntry {
                namespace: source_ns.to_string(),
                name: source_name.to_string(),
                checksum,
                restored,
                secret_name,
            })
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            let secret = create_backup_secret(
                sanitized_obj,
                source_ns,
                source_name,
                &checksum,
                backup_namespace,
            );
            api.create(&Default::default(), &secret)
                .await
                .map_err(|e| {
                    MigrationError::Kube(format!("create backup {secret_name}"), Box::new(e))
                })?;

            Ok(BackupEntry {
                namespace: source_ns.to_string(),
                name: source_name.to_string(),
                checksum,
                restored: false,
                secret_name,
            })
        }
        Err(e) => Err(MigrationError::Kube(
            format!("get backup {secret_name}"),
            Box::new(e),
        )),
    }
}

pub async fn list_backup_entries(api: &Api<Secret>) -> Result<Vec<BackupEntry>> {
    let label_selector = format!("{MIGRATION_LABEL}={MIGRATION_VERSION}");
    let secret_list = api
        .list(&kube::api::ListParams::default().labels(&label_selector))
        .await
        .map_err(|e| MigrationError::Kube("list backup secrets".to_string(), Box::new(e)))?;

    let mut entries = Vec::new();
    let mut seen_source_keys = std::collections::HashSet::new();
    let mut seen_secret_names = std::collections::HashSet::new();

    for secret in secret_list.items {
        let secret_name = secret
            .metadata
            .name
            .clone()
            .ok_or_else(|| MigrationError::Backup("backup secret missing name".to_string()))?;

        if !seen_secret_names.insert(secret_name.clone()) {
            return Err(MigrationError::Backup(format!(
                "duplicate backup secret name: {secret_name}"
            )));
        }

        let source_ref = secret
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(MIGRATION_SOURCE_ANNOTATION))
            .ok_or_else(|| {
                MigrationError::Backup(format!(
                    "backup secret {secret_name} missing source annotation"
                ))
            })?;

        if source_ref.is_empty() {
            return Err(MigrationError::Backup(format!(
                "backup secret {secret_name} has empty source annotation"
            )));
        }

        let (ns, name) = source_ref.split_once('/').ok_or_else(|| {
            MigrationError::Backup(format!(
                "backup secret {secret_name} has malformed source annotation: {source_ref}"
            ))
        })?;

        if ns.is_empty() || name.is_empty() {
            return Err(MigrationError::Backup(format!(
                "backup secret {secret_name} has empty namespace or name in source annotation"
            )));
        }

        let source_key = format!("{ns}/{name}");
        if !seen_source_keys.insert(source_key.clone()) {
            return Err(MigrationError::Backup(format!(
                "duplicate backup source: {source_key}"
            )));
        }

        let checksum = secret
            .string_data
            .as_ref()
            .and_then(|d| d.get("checksum"))
            .cloned()
            .or_else(|| {
                secret
                    .data
                    .as_ref()
                    .and_then(|d| d.get("checksum"))
                    .and_then(|v| String::from_utf8(v.0.clone()).ok())
            })
            .ok_or_else(|| {
                MigrationError::Backup(format!("backup secret {secret_name} missing checksum"))
            })?;

        if checksum.is_empty() {
            return Err(MigrationError::Backup(format!(
                "backup secret {secret_name} has empty checksum"
            )));
        }

        let obj_json = secret
            .string_data
            .as_ref()
            .and_then(|d| d.get("object.json"))
            .cloned()
            .or_else(|| {
                secret
                    .data
                    .as_ref()
                    .and_then(|d| d.get("object.json"))
                    .and_then(|v| String::from_utf8(v.0.clone()).ok())
            })
            .ok_or_else(|| {
                MigrationError::Backup(format!("backup secret {secret_name} missing object.json"))
            })?;

        let obj_value: serde_json::Value = serde_json::from_str(&obj_json).map_err(|e| {
            MigrationError::Serialization(format!("parse object.json from {secret_name}"), e)
        })?;

        let computed_checksum = crate::checksum::object_checksum(&obj_value);
        if computed_checksum != checksum {
            return Err(MigrationError::ChecksumMismatch {
                ns: ns.to_string(),
                name: name.to_string(),
                expected: checksum,
                actual: computed_checksum,
            });
        }

        let restored = secret
            .string_data
            .as_ref()
            .and_then(|d| d.get("restored"))
            .map(|v| v == "true")
            .or_else(|| {
                secret
                    .data
                    .as_ref()
                    .and_then(|d| d.get("restored"))
                    .and_then(|v| String::from_utf8(v.0.clone()).ok())
                    .map(|s| s == "true")
            })
            .unwrap_or(false);

        entries.push(BackupEntry {
            namespace: ns.to_string(),
            name: name.to_string(),
            checksum,
            restored,
            secret_name,
        });
    }

    Ok(entries)
}

pub async fn mark_backup_restored(api: &Api<Secret>, secret_name: &str) -> Result<()> {
    const MAX_RETRIES: usize = 5;

    for attempt in 0..MAX_RETRIES {
        let mut secret = api
            .get(secret_name)
            .await
            .map_err(|e| MigrationError::Kube(format!("get backup {secret_name}"), Box::new(e)))?;

        let already_restored = secret
            .string_data
            .as_ref()
            .and_then(|d| d.get("restored"))
            .map(|v| v == "true")
            .or_else(|| {
                secret
                    .data
                    .as_ref()
                    .and_then(|d| d.get("restored"))
                    .and_then(|v| String::from_utf8(v.0.clone()).ok())
                    .map(|s| s == "true")
            })
            .unwrap_or(false);

        if already_restored {
            return Ok(());
        }

        if let Some(ref mut data) = secret.string_data {
            data.insert("restored".to_string(), "true".to_string());
        } else if let Some(ref mut data) = secret.data {
            data.insert(
                "restored".to_string(),
                k8s_openapi::ByteString(b"true".to_vec()),
            );
        } else {
            secret.string_data = Some({
                let mut m = std::collections::BTreeMap::new();
                m.insert("restored".to_string(), "true".to_string());
                m
            });
        }

        match api.replace(secret_name, &Default::default(), &secret).await {
            Ok(_) => return Ok(()),
            Err(kube::Error::Api(ae)) if ae.code == 409 && attempt + 1 < MAX_RETRIES => {
                tracing::debug!(
                    secret_name,
                    attempt = attempt + 1,
                    "conflict marking backup restored; retrying"
                );
            }
            Err(e) => {
                return Err(MigrationError::Kube(
                    format!("update backup {secret_name}"),
                    Box::new(e),
                ));
            }
        }
    }

    Err(MigrationError::Backup(format!(
        "exhausted {MAX_RETRIES} retries marking backup {secret_name} as restored"
    )))
}

pub async fn delete_backup(api: &Api<Secret>, secret_name: &str) -> Result<()> {
    match api.delete(secret_name, &Default::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(MigrationError::Kube(
            format!("delete backup {secret_name}"),
            Box::new(e),
        )),
    }
}

pub async fn delete_all_backups(api: &Api<Secret>) -> Result<()> {
    let entries = list_backup_entries(api).await?;
    for entry in entries {
        delete_backup(api, &entry.secret_name).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_create_backup_secret_structure() {
        let obj = json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let secret = create_backup_secret(&obj, "default", "alice", "abc123", "kaniop");

        assert!(
            secret
                .metadata
                .name
                .as_ref()
                .unwrap()
                .starts_with(BACKUP_PREFIX)
        );
        assert_eq!(secret.metadata.namespace.as_ref().unwrap(), "kaniop");

        let labels = secret.metadata.labels.as_ref().unwrap();
        assert_eq!(labels.get(MIGRATION_LABEL).unwrap(), MIGRATION_VERSION);

        let annotations = secret.metadata.annotations.as_ref().unwrap();
        assert_eq!(
            annotations.get(MIGRATION_SOURCE_ANNOTATION).unwrap(),
            "default/alice"
        );

        let data = secret.string_data.as_ref().unwrap();
        assert_eq!(data.get("checksum").unwrap(), "abc123");
        assert_eq!(data.get("restored").unwrap(), "false");
        assert!(data.get("object.json").is_some());
    }

    #[test]
    fn test_backup_secret_name_is_deterministic() {
        let name1 = backup_secret_name(BACKUP_PREFIX, "default", "alice");
        let name2 = backup_secret_name(BACKUP_PREFIX, "default", "alice");
        assert_eq!(name1, name2);
    }

    #[test]
    fn test_backup_secret_name_differs_for_different_sources() {
        let name1 = backup_secret_name(BACKUP_PREFIX, "default", "alice");
        let name2 = backup_secret_name(BACKUP_PREFIX, "default", "bob");
        let name3 = backup_secret_name(BACKUP_PREFIX, "other", "alice");
        assert_ne!(name1, name2);
        assert_ne!(name1, name3);
    }

    #[test]
    fn test_validate_backup_size_under_limit() {
        let obj = json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let result = validate_backup_size(&obj, "default", "alice");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_backup_size_over_limit() {
        let large_data = "x".repeat(MAX_BACKUP_SECRET_SIZE + 1000);
        let obj = json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}, "largeField": large_data}
        });

        let result = validate_backup_size(&obj, "default", "alice");
        assert!(result.is_err());
        match result.unwrap_err() {
            MigrationError::Backup(msg) => {
                assert!(msg.contains("exceeds size limit"));
                assert!(msg.contains("default/alice"));
            }
            e => panic!("expected Backup error, got {:?}", e),
        }
    }

    #[test]
    fn test_validate_backup_size_at_boundary() {
        let obj = json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {"name": "alice", "namespace": "default"},
            "spec": {"kanidmRef": {"name": "kanidm"}}
        });

        let serialized = serde_json::to_vec(&obj).unwrap();
        let base_size = serialized.len();

        if base_size < MAX_BACKUP_SECRET_SIZE {
            let padding_needed = MAX_BACKUP_SECRET_SIZE - base_size - 50;
            let padding = "x".repeat(padding_needed);
            let obj_at_limit = json!({
                "apiVersion": "kaniop.rs/v1beta1",
                "kind": "KanidmPersonAccount",
                "metadata": {"name": "alice", "namespace": "default"},
                "spec": {"kanidmRef": {"name": "kanidm"}, "padding": padding}
            });

            let result = validate_backup_size(&obj_at_limit, "default", "alice");
            assert!(result.is_ok(), "object under limit should pass validation");

            let serialized_check = serde_json::to_vec(&obj_at_limit).unwrap();
            assert!(
                serialized_check.len() <= MAX_BACKUP_SECRET_SIZE,
                "test object should be under limit: {} <= {}",
                serialized_check.len(),
                MAX_BACKUP_SECRET_SIZE
            );
        }
    }
}
