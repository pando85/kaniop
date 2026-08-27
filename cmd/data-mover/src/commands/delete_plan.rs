use std::time::Duration;

use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DeletionResult, ExitCode, FailedKey, ResultDocument};
use s3::bucket::Bucket;
use tracing::{error, info, warn};

use crate::s3::{S3Config, create_bucket};

use super::{load_operation, write_result};

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

    info!(
        bucket = %op.bucket,
        key_count = op.keys.len(),
        "starting delete-plan"
    );

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    for key in &op.keys {
        if !repo_path.contains_key(key) {
            error!(key = %key, "key escapes repository prefix");
            return Err(ExitCode::InvalidInput as i32);
        }
    }

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

    let manifest_keys: Vec<&String> = op
        .keys
        .iter()
        .filter(|k| k.ends_with("/manifest.json"))
        .collect();
    let other_keys: Vec<&String> = op
        .keys
        .iter()
        .filter(|k| !k.ends_with("/manifest.json"))
        .collect();

    let mut deleted_keys = Vec::new();
    let mut failed_keys = Vec::new();

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
