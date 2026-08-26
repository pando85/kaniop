use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DiscoverResult, ExitCode, ResultDocument};
use s3::bucket::Bucket;
use tracing::{error, info, warn};

use crate::s3::{S3Config, S3Error, create_bucket, list_objects_page};

use super::{load_operation, write_result};

const LIST_PAGE_SIZE: usize = 100;
const MAX_PAGES: u32 = 100;

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Discover(op) => op,
        _ => {
            error!("expected discover operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();

    info!(
        bucket = %op.bucket,
        namespace_uid = %op.namespace_uid,
        kanidm_uid = %op.kanidm_uid,
        max_results = op.max_results,
        "starting discover"
    );

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    let manifests_prefix = repo_path
        .manifests_prefix(&op.namespace_uid, &op.kanidm_uid)
        .map_err(|e| {
            error!(error = %e, "failed to construct manifests prefix");
            ExitCode::InvalidInput as i32
        })?;

    let s3_config = S3Config {
        bucket: op.bucket.clone(),
        endpoint: op.endpoint.clone(),
        region: op.region.clone(),
        force_path_style: op.force_path_style,
        ca_bundle_path: op.ca_bundle_path.clone(),
    };

    let bucket = create_bucket(&s3_config).await.map_err(|e| {
        error!(error = %e, "failed to create S3 client");
        ExitCode::Retryable as i32
    })?;

    let manifest_keys = discover_manifest_keys(
        &bucket,
        &manifests_prefix,
        op.max_results as usize,
        op.max_retries,
    )
    .await?;

    let truncated = manifest_keys.len() >= op.max_results as usize;

    info!(
        found = manifest_keys.len(),
        truncated = truncated,
        "discover completed"
    );

    let mut result = ResultDocument::success("discover");
    result.discovery = Some(DiscoverResult {
        manifest_keys: manifest_keys.clone(),
        total_found: manifest_keys.len() as u32,
        truncated,
    });

    write_result(&result_path, &result).await?;

    Ok(())
}

fn is_valid_manifest_key(key: &str) -> bool {
    key.ends_with("/manifest.json") && !key.contains("..")
}

async fn discover_manifest_keys(
    bucket: &Bucket,
    prefix: &str,
    max_results: usize,
    max_retries: u32,
) -> Result<Vec<String>, i32> {
    let mut manifest_keys = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut pages_fetched = 0u32;

    loop {
        if manifest_keys.len() >= max_results {
            break;
        }
        if pages_fetched >= MAX_PAGES {
            warn!(
                pages_fetched,
                max_pages = MAX_PAGES,
                "reached max pages limit; results may be truncated"
            );
            break;
        }

        let remaining = max_results - manifest_keys.len();
        let page_size = std::cmp::min(LIST_PAGE_SIZE, remaining);

        let page_result = list_with_retry(
            bucket,
            prefix,
            continuation_token.clone(),
            page_size,
            max_retries,
        )
        .await?;
        let (list_result, _status) = page_result;

        for obj in &list_result.contents {
            if is_valid_manifest_key(&obj.key) {
                manifest_keys.push(obj.key.clone());
            } else if obj.key.ends_with("/manifest.json") {
                warn!(key = %obj.key, "skipping key with path traversal");
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

    Ok(manifest_keys)
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
            let backoff = std::time::Duration::from_secs(2u64.pow(attempt).min(30));
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

    let err = last_error.unwrap_or_else(|| S3Error::Operation("unknown error".to_string()));
    error!(error = %err, "list objects failed after retries");
    Err(ExitCode::Retryable as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_key_filter_accepts_valid_keys() {
        assert!(is_valid_manifest_key(
            "prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json"
        ));
        assert!(!is_valid_manifest_key(
            "prod/v1/tenants/ns/clusters/k/backups/b1/payload/data.json"
        ));
    }

    #[test]
    fn path_traversal_keys_are_rejected() {
        assert!(!is_valid_manifest_key(
            "prod/v1/tenants/../clusters/k/backups/b1/manifest.json"
        ));
    }
}
