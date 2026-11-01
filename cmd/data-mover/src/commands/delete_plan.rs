use std::time::Duration;

use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DeletionResult, ExitCode, FailedKey, ResultDocument};
use s3::bucket::Bucket;
use s3::serde_types::ObjectIdentifier;
use tracing::{error, info, warn};

use crate::s3::{S3Config, create_bucket, list_objects_page};

use super::{load_operation, write_result};

const LIST_PAGE_SIZE: usize = 100;
const MAX_LIST_PAGES: u32 = 100;

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::DeletePlan(op) => op,
        _ => {
            error!("expected delete-plan operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    let s3_config = S3Config {
        bucket: op.bucket.clone(),
        endpoint: op.endpoint.clone(),
        region: op.region.clone(),
        force_path_style: op.force_path_style,
        ca_bundle_path: op.ca_bundle_path.clone(),
        insecure: op.insecure,
    };

    let bucket = create_bucket(&s3_config).await.map_err(|e| {
        error!(error = %e, "failed to create S3 client");
        ExitCode::Retryable as i32
    })?;

    let mut deleted_keys = Vec::new();
    let mut failed_keys = Vec::new();

    let keys_to_delete = if let Some(backup_prefix) = &op.backup_prefix {
        if !repo_path.contains_prefix(backup_prefix) {
            error!(prefix = %backup_prefix, "backup prefix escapes repository");
            return Err(ExitCode::InvalidInput as i32);
        }
        info!(
            bucket = %op.bucket,
            backup_prefix = %backup_prefix,
            "starting delete-plan (prefix mode)"
        );
        list_all_versions_under_prefix(&bucket, backup_prefix, op.max_retries, &mut failed_keys)
            .await?
    } else if let Some(keys) = &op.keys {
        for key in keys {
            if !repo_path.contains_key(key) {
                error!(key = %key, "key escapes repository prefix");
                return Err(ExitCode::InvalidInput as i32);
            }
        }
        info!(
            bucket = %op.bucket,
            key_count = keys.len(),
            "starting delete-plan (keys mode)"
        );
        let mut versions = Vec::new();
        for key in keys {
            match get_version_id(&bucket, key, op.max_retries).await {
                Ok(version_id) => versions.push(VersionedKey {
                    key: key.clone(),
                    version_id,
                }),
                Err(reason) => {
                    warn!(key = %key, reason = %reason, "head_object failed; marking key as failed");
                    failed_keys.push(FailedKey {
                        key: key.clone(),
                        reason,
                    });
                }
            }
        }
        versions
    } else {
        error!("delete-plan has neither keys nor backupPrefix");
        return Err(ExitCode::InvalidInput as i32);
    };

    let manifest_keys: Vec<VersionedKey> = keys_to_delete
        .iter()
        .filter(|k| k.key.ends_with("/manifest.json"))
        .cloned()
        .collect();
    let other_keys: Vec<VersionedKey> = keys_to_delete
        .iter()
        .filter(|k| !k.key.ends_with("/manifest.json"))
        .cloned()
        .collect();

    delete_versioned_keys(
        &bucket,
        &manifest_keys,
        op.max_retries,
        &mut deleted_keys,
        &mut failed_keys,
    )
    .await;
    delete_versioned_keys(
        &bucket,
        &other_keys,
        op.max_retries,
        &mut deleted_keys,
        &mut failed_keys,
    )
    .await;

    info!(
        deleted = deleted_keys.len(),
        failed = failed_keys.len(),
        "delete-plan completed"
    );

    let has_failures = !failed_keys.is_empty();

    let mut result = ResultDocument::success("delete-plan");
    result.deletion = Some(DeletionResult {
        deleted_keys,
        failed_keys,
    });

    write_result(&result_path, &result).await?;

    if has_failures {
        warn!("some keys failed to delete");
        return Err(ExitCode::Retryable as i32);
    }

    Ok(())
}

#[derive(Clone)]
struct VersionedKey {
    key: String,
    version_id: Option<String>,
}

async fn get_version_id(
    bucket: &Bucket,
    key: &str,
    max_retries: u32,
) -> Result<Option<String>, String> {
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(30));
            tokio::time::sleep(backoff).await;
        }
        match bucket.head_object(key).await {
            Ok((result, _status)) => return Ok(result.version_id),
            Err(e) => {
                warn!(key = %key, attempt, error = %e, "head_object failed");
            }
        }
    }
    Err(format!(
        "head_object failed after {} retries",
        max_retries + 1
    ))
}

async fn list_all_versions_under_prefix(
    bucket: &Bucket,
    prefix: &str,
    max_retries: u32,
    failed: &mut Vec<FailedKey>,
) -> Result<Vec<VersionedKey>, i32> {
    let mut all_versions = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut pages_fetched = 0u32;

    loop {
        if pages_fetched >= MAX_LIST_PAGES {
            error!(
                pages_fetched,
                max_pages = MAX_LIST_PAGES,
                "reached max list pages; refusing to delete with truncated listing"
            );
            return Err(ExitCode::Retryable as i32);
        }

        let page_result = list_with_retry(
            bucket,
            prefix,
            continuation_token.clone(),
            LIST_PAGE_SIZE,
            max_retries,
        )
        .await?;
        let (list_result, _status) = page_result;

        for obj in &list_result.contents {
            match get_version_id(bucket, &obj.key, max_retries).await {
                Ok(version_id) => all_versions.push(VersionedKey {
                    key: obj.key.clone(),
                    version_id,
                }),
                Err(reason) => {
                    warn!(key = %obj.key, reason = %reason, "head_object failed; marking key as failed");
                    failed.push(FailedKey {
                        key: obj.key.clone(),
                        reason,
                    });
                }
            }
        }

        pages_fetched += 1;

        if !list_result.is_truncated {
            break;
        }

        continuation_token = list_result.next_continuation_token;
        if continuation_token.is_none() {
            break;
        }
    }

    Ok(all_versions)
}

async fn delete_versioned_keys(
    bucket: &Bucket,
    keys: &[VersionedKey],
    max_retries: u32,
    deleted: &mut Vec<String>,
    failed: &mut Vec<FailedKey>,
) {
    if keys.is_empty() {
        return;
    }

    let identifiers: Vec<ObjectIdentifier> = keys
        .iter()
        .map(|vk| match &vk.version_id {
            Some(vid) => ObjectIdentifier::with_version(&vk.key, vid),
            None => ObjectIdentifier::new(&vk.key),
        })
        .collect();

    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(30));
            info!(attempt, ?backoff, "retrying delete_objects");
            tokio::time::sleep(backoff).await;
        }

        match bucket.delete_objects(identifiers.clone()).await {
            Ok(result) => {
                for d in &result.deleted {
                    info!(key = %d.key, "deleted");
                    deleted.push(d.key.clone());
                }
                for e in &result.errors {
                    let reason = format!("{}: {}", e.code, e.message);
                    if reason.contains("AccessDenied") || reason.contains("ObjectLock") {
                        warn!(key = %e.key, reason = %reason, "deletion denied");
                        failed.push(FailedKey {
                            key: e.key.clone(),
                            reason,
                        });
                    } else {
                        warn!(key = %e.key, reason = %reason, "deletion failed");
                        failed.push(FailedKey {
                            key: e.key.clone(),
                            reason,
                        });
                    }
                }
                return;
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("AccessDenied") || err_str.contains("ObjectLock") {
                    warn!(error = %e, "delete_objects denied");
                    for vk in keys {
                        failed.push(FailedKey {
                            key: vk.key.clone(),
                            reason: err_str.clone(),
                        });
                    }
                    return;
                }
                warn!(attempt, error = %e, "delete_objects failed");
            }
        }
    }

    for vk in keys {
        warn!(key = %vk.key, "deletion failed after retries");
        failed.push(FailedKey {
            key: vk.key.clone(),
            reason: "failed after retries".to_string(),
        });
    }
}

async fn list_with_retry(
    bucket: &Bucket,
    prefix: &str,
    continuation_token: Option<String>,
    max_keys: usize,
    max_retries: u32,
) -> Result<(s3::serde_types::ListBucketResult, u16), i32> {
    let mut last_error = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(30));
            info!(attempt, ?backoff, "retrying list objects");
            tokio::time::sleep(backoff).await;
        }

        match list_objects_page(bucket, prefix, continuation_token.clone(), max_keys).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    let err =
        last_error.unwrap_or_else(|| crate::s3::S3Error::Operation("unknown error".to_string()));
    error!(error = %err, "list objects failed after retries");
    Err(ExitCode::Retryable as i32)
}

#[cfg(test)]
mod tests {
    use kaniop_backup_core::result::{DeletionResult, FailedKey};

    #[test]
    fn partition_keys_separates_manifests_from_payloads() {
        let keys = [
            "p/v1/tenants/ns/clusters/k/backups/b1/manifest.json".to_string(),
            "p/v1/tenants/ns/clusters/k/backups/b1/payload/data.gz".to_string(),
        ];
        let manifests: Vec<&String> = keys
            .iter()
            .filter(|k| k.ends_with("/manifest.json"))
            .collect();
        let payloads: Vec<&String> = keys
            .iter()
            .filter(|k| !k.ends_with("/manifest.json"))
            .collect();
        assert_eq!(manifests.len(), 1);
        assert_eq!(payloads.len(), 1);
    }

    #[test]
    fn object_lock_reason_is_classifiable_by_controller() {
        let dr = DeletionResult {
            deleted_keys: vec![],
            failed_keys: vec![FailedKey {
                key: "k".to_string(),
                reason: "ObjectLock: retention period not expired".to_string(),
            }],
        };
        assert!(dr.failed_keys[0].is_object_lock());
        assert_eq!(
            dr.classify_deferral(),
            Some(kaniop_backup_core::result::GcDeferReason::ObjectLock)
        );
    }

    #[test]
    fn access_denied_reason_is_classifiable_by_controller() {
        let dr = DeletionResult {
            deleted_keys: vec![],
            failed_keys: vec![FailedKey {
                key: "k".to_string(),
                reason: "AccessDenied: insufficient permissions".to_string(),
            }],
        };
        assert!(dr.failed_keys[0].is_access_denied());
        assert_eq!(
            dr.classify_deferral(),
            Some(kaniop_backup_core::result::GcDeferReason::AccessDenied)
        );
    }

    #[test]
    fn head_object_failure_reason_is_reported_as_failed_key() {
        let reason = "head_object failed after 4 retries".to_string();
        let fk = FailedKey {
            key: "p/v1/tenants/ns/clusters/k/backups/b1/payload/data.gz".to_string(),
            reason: reason.clone(),
        };
        assert!(!fk.is_object_lock());
        assert!(!fk.is_access_denied());
        let dr = DeletionResult {
            deleted_keys: vec![],
            failed_keys: vec![fk],
        };
        assert!(dr.classify_deferral().is_none());
        assert_eq!(dr.failed_keys[0].reason, reason);
    }

    #[test]
    fn versioned_key_without_version_id_uses_versionless_identifier() {
        let vk_with = super::VersionedKey {
            key: "k1".to_string(),
            version_id: Some("v1".to_string()),
        };
        let vk_without = super::VersionedKey {
            key: "k2".to_string(),
            version_id: None,
        };
        assert!(vk_with.version_id.is_some());
        assert!(vk_without.version_id.is_none());
    }
}
