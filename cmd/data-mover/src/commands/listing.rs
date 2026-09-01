use std::time::Duration;

use s3::bucket::Bucket;
use tracing::{error, info, warn};

use crate::s3::{S3Error, list_objects_page};

const LIST_PAGE_SIZE: usize = 100;
const MAX_PAGES: u32 = 100;

pub fn is_valid_manifest_key(key: &str) -> bool {
    key.ends_with("/manifest.json") && !key.contains("..")
}

pub fn extract_backup_id_from_manifest_key(key: &str) -> Option<&str> {
    if !is_valid_manifest_key(key) {
        return None;
    }
    let key_before_manifest = key.strip_suffix("/manifest.json")?;
    let backup_id = key_before_manifest.rsplit('/').next()?;
    if backup_id.is_empty() {
        None
    } else {
        Some(backup_id)
    }
}

pub async fn list_manifest_keys(
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

    let err = last_error.unwrap_or_else(|| S3Error::Operation("unknown error".to_string()));
    error!(error = %err, "list objects failed after retries");
    Err(kaniop_backup_core::result::ExitCode::Retryable as i32)
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

    #[test]
    fn extract_backup_id_from_valid_key() {
        let key = "prod/v1/tenants/ns/clusters/k/backups/019c7c76-f423-7a12-8f41-2bea7588a303/manifest.json";
        assert_eq!(
            extract_backup_id_from_manifest_key(key),
            Some("019c7c76-f423-7a12-8f41-2bea7588a303")
        );
    }

    #[test]
    fn extract_backup_id_from_invalid_key_returns_none() {
        assert_eq!(extract_backup_id_from_manifest_key("not/a/manifest"), None);
        assert_eq!(extract_backup_id_from_manifest_key(""), None);
    }

    #[test]
    fn extract_backup_id_from_traversal_key_returns_none() {
        let key = "prod/v1/tenants/../clusters/k/backups/b1/manifest.json";
        assert_eq!(extract_backup_id_from_manifest_key(key), None);
    }
}
