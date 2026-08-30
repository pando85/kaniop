use std::time::Duration;

use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DeletionResult, ExitCode, FailedKey, ResultDocument};
use s3::bucket::Bucket;
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
        list_all_keys_under_prefix(&bucket, backup_prefix, op.max_retries).await?
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
        keys.clone()
    } else {
        error!("delete-plan has neither keys nor backupPrefix");
        return Err(ExitCode::InvalidInput as i32);
    };

    let mut deleted_keys = Vec::new();
    let mut failed_keys = Vec::new();

    let manifest_keys: Vec<&String> = keys_to_delete
        .iter()
        .filter(|k| k.ends_with("/manifest.json"))
        .collect();
    let other_keys: Vec<&String> = keys_to_delete
        .iter()
        .filter(|k| !k.ends_with("/manifest.json"))
        .collect();

    for key in manifest_keys {
        delete_key(
            &bucket,
            key,
            op.max_retries,
            &mut deleted_keys,
            &mut failed_keys,
        )
        .await;
    }

    for key in other_keys {
        delete_key(
            &bucket,
            key,
            op.max_retries,
            &mut deleted_keys,
            &mut failed_keys,
        )
        .await;
    }

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

async fn list_all_keys_under_prefix(
    bucket: &Bucket,
    prefix: &str,
    max_retries: u32,
) -> Result<Vec<String>, i32> {
    let mut all_keys = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut pages_fetched = 0u32;

    loop {
        if pages_fetched >= MAX_LIST_PAGES {
            warn!(
                pages_fetched,
                max_pages = MAX_LIST_PAGES,
                "reached max list pages; some objects may not be deleted"
            );
            break;
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
            all_keys.push(obj.key.clone());
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

    Ok(all_keys)
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

async fn delete_key(
    bucket: &Bucket,
    key: &str,
    max_retries: u32,
    deleted: &mut Vec<String>,
    failed: &mut Vec<FailedKey>,
) {
    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(30));
            tokio::time::sleep(backoff).await;
        }

        match bucket.delete_object(key).await {
            Ok(_) => {
                info!(key = %key, "deleted");
                deleted.push(key.to_string());
                return;
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("AccessDenied") || err_str.contains("ObjectLock") {
                    warn!(key = %key, error = %e, "deletion denied");
                    failed.push(FailedKey {
                        key: key.to_string(),
                        reason: err_str,
                    });
                    return;
                }
                last_error = Some(err_str);
            }
        }
    }

    let err = last_error.unwrap_or_default();
    warn!(key = %key, error = %err, "deletion failed after retries");
    failed.push(FailedKey {
        key: key.to_string(),
        reason: err,
    });
}

#[cfg(test)]
mod tests {
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
}
