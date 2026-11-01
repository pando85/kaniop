use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{DiscoverResult, ExitCode, ResultDocument};
use tracing::{error, info};

use crate::s3::{S3Config, create_bucket};

use super::listing::list_manifest_keys;
use super::{load_operation, write_result};

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
        insecure: op.insecure,
    };

    let bucket = create_bucket(&s3_config).await.map_err(|e| {
        error!(error = %e, "failed to create S3 client");
        ExitCode::Retryable as i32
    })?;

    let list_result = list_manifest_keys(
        &bucket,
        &manifests_prefix,
        op.max_results as usize,
        op.max_retries,
    )
    .await?;

    let manifest_keys = list_result.keys;
    let incomplete = list_result.incomplete;
    let truncated = discover_truncated(manifest_keys.len(), op.max_results as usize, incomplete);

    info!(
        found = manifest_keys.len(),
        truncated = truncated,
        incomplete = incomplete,
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

/// Returns true when the discover listing may be incomplete.
///
/// A listing is truncated when either the caller-supplied result cap was
/// reached (`key_count >= max_results`) or `list_manifest_keys` could not cover the whole
/// prefix (`incomplete`: the page cap was hit, or the provider gave no continuation token).
fn discover_truncated(key_count: usize, max_results: usize, incomplete: bool) -> bool {
    key_count >= max_results || incomplete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_when_listing_incomplete_below_result_cap() {
        assert!(discover_truncated(50, 1000, true));
    }

    #[test]
    fn truncated_when_result_cap_reached() {
        assert!(discover_truncated(1000, 1000, false));
    }

    #[test]
    fn truncated_when_result_cap_and_incomplete_both_set() {
        assert!(discover_truncated(1000, 1000, true));
    }

    #[test]
    fn not_truncated_when_complete_below_result_cap() {
        assert!(!discover_truncated(50, 1000, false));
    }

    #[test]
    fn truncated_when_max_results_is_zero() {
        // max_results == 0 is not reachable in practice (the operator rejects it),
        // but the helper must still be well-defined: 0 >= 0 is true.
        // a zero cap proves no coverage, and the prune trusts `truncated == false` alone.
        assert!(discover_truncated(0, 0, false));
    }

    #[test]
    fn incomplete_flag_alone_truncates_even_with_zero_keys() {
        assert!(discover_truncated(0, 1000, true));
    }

    #[test]
    fn incomplete_flag_truncates_below_both_caps() {
        // The case the prune depends on: a partial listing that stops well under the result cap
        // (here because the provider gave no continuation token) must still report truncated,
        // or `snapshot_supports_pruning` would read absence into keys the LIST never returned.
        assert!(discover_truncated(3, 1000, true));
    }

    #[test]
    fn key_count_exceeding_max_results_is_truncated() {
        // Defensive: the loop breaks at >= but a caller could in theory
        // pass a count above the cap.
        assert!(discover_truncated(1001, 1000, false));
    }
}
