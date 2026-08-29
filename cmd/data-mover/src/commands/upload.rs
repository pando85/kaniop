use std::path::Path;

use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use tracing::{error, info};

use crate::checksum;
use crate::s3::{S3Config, create_bucket};

use super::upload_shared::{
    ManifestParams, build_manifest, upload_manifest_conditional, upload_payload_streaming,
    verify_commit,
};
use super::{load_operation, write_result};

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Upload(op) => op,
        _ => {
            error!("expected upload operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();
    let payload_path = Path::new(&op.payload_path);

    if !payload_path.exists() {
        let result = ResultDocument::failure(
            "upload",
            ExitCode::InvalidInput,
            "PAYLOAD_NOT_FOUND",
            &format!("payload file not found: {}", op.payload_path),
        );
        let _ = write_result(&result_path, &result).await;
        return Err(ExitCode::InvalidInput as i32);
    }

    info!(payload = %op.payload_path, backup_id = %op.backup_id, "starting upload");

    let local_checksum = checksum::compute_sha256(payload_path).await.map_err(|e| {
        error!(error = %e, "failed to compute payload checksum");
        ExitCode::Retryable as i32
    })?;

    info!(
        sha256 = %local_checksum.sha256,
        size = local_checksum.size_bytes,
        "payload checksum computed"
    );

    let repo_path = RepositoryPath::new(&op.bucket, &op.prefix).map_err(|e| {
        error!(error = %e, "invalid repository path");
        ExitCode::InvalidInput as i32
    })?;

    let payload_key = repo_path
        .payload_key(
            &op.namespace_uid,
            &op.kanidm_uid,
            &op.backup_id,
            payload_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "kanidm.backup.json".to_string())
                .as_ref(),
        )
        .map_err(|e| {
            error!(error = %e, "failed to construct payload key");
            ExitCode::InvalidInput as i32
        })?;

    let manifest_key = repo_path
        .manifest_key(&op.namespace_uid, &op.kanidm_uid, &op.backup_id)
        .map_err(|e| {
            error!(error = %e, "failed to construct manifest key");
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

    upload_payload_streaming(
        &bucket,
        payload_path,
        &payload_key,
        op.max_retries,
        op.max_concurrent_parts,
    )
    .await?;

    info!(key = %payload_key, "payload uploaded");

    let params = ManifestParams {
        backup_id: &op.backup_id,
        namespace_uid: &op.namespace_uid,
        kanidm_uid: &op.kanidm_uid,
        kanidm_name: &op.kanidm_name,
        domain: &op.domain,
        kanidm_version: &op.kanidm_version,
        image_digest: op.image_digest.as_deref(),
        consistency: &op.consistency,
        reason: &op.reason,
        encryption_mode: op.encryption_mode.as_deref(),
        encryption_key_id: op.encryption_key_id.as_deref(),
    };

    let manifest = build_manifest(&params, &payload_key, &local_checksum);
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
        error!(error = %e, "failed to serialize manifest");
        ExitCode::Retryable as i32
    })?;

    upload_manifest_conditional(&bucket, &manifest_key, &manifest_json, op.max_retries).await?;

    info!(key = %manifest_key, "manifest uploaded (conditional create)");

    verify_commit(&bucket, &manifest_key, &manifest_json).await?;

    info!("commit verified");

    let mut result = ResultDocument::success("upload");
    result.backup_id = Some(op.backup_id.clone());
    result.manifest_key = Some(manifest_key);
    result.payload_key = Some(payload_key);
    result.payload_sha256 = Some(local_checksum.sha256);
    result.payload_size_bytes = Some(local_checksum.size_bytes);

    write_result(&result_path, &result).await?;

    info!(backup_id = %op.backup_id, "upload completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use kaniop_backup_core::manifest::{MANIFEST_API_VERSION_V1, MANIFEST_KIND};
    use kaniop_backup_core::operation::UploadOperation;

    use super::*;

    fn test_upload_op() -> UploadOperation {
        UploadOperation {
            payload_path: "/tmp/test.bin".to_string(),
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
            result_path: "/tmp/result.json".to_string(),
            max_concurrent_parts: 4,
            max_retries: 3,
        }
    }

    #[test]
    fn build_manifest_from_upload_op() {
        let op = test_upload_op();
        let checksum_result = checksum::ChecksumResult {
            sha256: "abc123".to_string(),
            size_bytes: 1024,
        };
        let payload_key = "prod/v1/tenants/ns-uid/clusters/k-uid/backups/019c7c76-f423-7a12-8f41-2bea7588a303/payload/backup.json";

        let params = ManifestParams {
            backup_id: &op.backup_id,
            namespace_uid: &op.namespace_uid,
            kanidm_uid: &op.kanidm_uid,
            kanidm_name: &op.kanidm_name,
            domain: &op.domain,
            kanidm_version: &op.kanidm_version,
            image_digest: op.image_digest.as_deref(),
            consistency: &op.consistency,
            reason: &op.reason,
            encryption_mode: op.encryption_mode.as_deref(),
            encryption_key_id: op.encryption_key_id.as_deref(),
        };

        let manifest = build_manifest(&params, payload_key, &checksum_result);

        assert_eq!(manifest.api_version, MANIFEST_API_VERSION_V1);
        assert_eq!(manifest.kind, MANIFEST_KIND);
        assert_eq!(manifest.backup_id, op.backup_id);
        assert_eq!(manifest.payload.sha256, "abc123");
        assert_eq!(manifest.payload.size_bytes, 1024);
        assert!(manifest.validate().is_ok());
    }
}
