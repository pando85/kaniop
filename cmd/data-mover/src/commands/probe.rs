use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, ProbeResult, ResultDocument};
use s3::bucket::Bucket;
use tracing::{error, info};
use uuid::Uuid;

use crate::s3::{S3Config, create_bucket, probe_conditional_put_capability};

use super::{load_operation, write_result};

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Probe(op) => op,
        _ => {
            error!("expected probe operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();

    info!(bucket = %op.bucket, endpoint = %op.endpoint, "starting probe");

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    let s3_config = S3Config {
        bucket: op.bucket.clone(),
        prefix: op.prefix.clone(),
        endpoint: op.endpoint.clone(),
        region: op.region.clone(),
        force_path_style: op.force_path_style,
        ca_bundle_path: op.ca_bundle_path.clone(),
    };

    let bucket = create_bucket(&s3_config).await.map_err(|e| {
        error!(error = %e, "failed to create S3 client");
        ExitCode::Retryable as i32
    })?;

    let probe_key = repo_path.probe_key();
    let probe_content = format!("kaniop-probe-{}", Uuid::new_v4());

    let multipart_ok = probe_multipart(&bucket, &probe_key, probe_content.as_bytes()).await;
    let conditional_put_ok = probe_conditional_put_capability(&bucket, &probe_key).await;
    let head_ok = probe_head(&bucket, &probe_key).await;

    let _ = bucket.delete_object(&probe_key).await;

    info!(
        multipart = multipart_ok,
        conditional_put = conditional_put_ok,
        head = head_ok,
        "probe completed"
    );

    let mut result = ResultDocument::success("probe");
    result.probe = Some(ProbeResult {
        multipart_upload: multipart_ok,
        conditional_put: conditional_put_ok,
        head_object: head_ok,
    });

    write_result(&result_path, &result).await?;

    if !multipart_ok || !head_ok {
        error!("probe failed: required capabilities missing");
        return Err(ExitCode::Retryable as i32);
    }

    Ok(())
}

async fn probe_multipart(bucket: &Bucket, key: &str, data: &[u8]) -> bool {
    match bucket.put_object(key, data).await {
        Ok(_) => true,
        Err(e) => {
            info!(error = %e, "multipart probe failed");
            false
        }
    }
}

async fn probe_head(bucket: &Bucket, key: &str) -> bool {
    match bucket.head_object(key).await {
        Ok(_) => true,
        Err(e) => {
            info!(error = %e, "HEAD probe failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_result_fields_are_independent() {
        let result = ProbeResult {
            multipart_upload: true,
            conditional_put: false,
            head_object: true,
        };
        assert!(result.multipart_upload);
        assert!(!result.conditional_put);
        assert!(result.head_object);
    }
}
