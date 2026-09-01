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

    let manifest_keys = list_manifest_keys(
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
