use std::path::Path;
use std::time::Duration;

use kaniop_backup_core::manifest::{
    KanidmBackupManifest, MANIFEST_API_VERSION_V1, MANIFEST_KIND, ManifestBackup,
    ManifestCompatibility, ManifestEncryption, ManifestPayload, ManifestSource,
};
use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use s3::bucket::Bucket;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader};
use tracing::{error, info, warn};

use crate::checksum;
use crate::s3::{DEFAULT_PART_SIZE, S3Config, S3Error, create_bucket};

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

    let manifest = build_manifest(op, &payload_key, &local_checksum);
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

async fn upload_payload_streaming(
    bucket: &Bucket,
    payload_path: &Path,
    key: &str,
    max_retries: u32,
    _max_concurrent_parts: u32,
) -> Result<(), i32> {
    let file_size = tokio::fs::metadata(payload_path)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to get file metadata");
            ExitCode::Retryable as i32
        })?
        .len();

    let part_size = DEFAULT_PART_SIZE;
    let use_multipart = file_size > part_size as u64;

    if !use_multipart {
        return upload_payload_simple(bucket, payload_path, key, max_retries).await;
    }

    info!(
        file_size = file_size,
        part_size = part_size,
        "using streaming multipart upload"
    );

    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying multipart payload upload");
            tokio::time::sleep(backoff).await;
        }

        match upload_multipart_streaming(bucket, payload_path, key, part_size, file_size).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(error = %e, attempt, "multipart upload attempt failed");
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap_or_else(|| "unknown error".to_string());
    error!(error = %err, "payload multipart upload failed after retries");
    Err(ExitCode::Retryable as i32)
}

async fn upload_multipart_streaming(
    bucket: &Bucket,
    payload_path: &Path,
    key: &str,
    part_size: usize,
    file_size: u64,
) -> Result<(), String> {
    let response = bucket
        .initiate_multipart_upload(key, "application/octet-stream")
        .await
        .map_err(|e| format!("initiate multipart upload failed: {e}"))?;

    let upload_id = &response.upload_id;
    let total_parts = file_size.div_ceil(part_size as u64) as u32;
    let mut parts = Vec::with_capacity(total_parts as usize);
    let mut upload_result = Ok(());

    for part_number in 1..=total_parts {
        let offset = (part_number - 1) as u64 * part_size as u64;
        let remaining = file_size - offset;
        let this_part_size = std::cmp::min(part_size as u64, remaining) as usize;

        let chunk_result = read_file_chunk(payload_path, offset, this_part_size).await;
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(e) => {
                upload_result = Err(format!("failed to read chunk at offset {offset}: {e}"));
                break;
            }
        };

        let part = match bucket
            .put_multipart_chunk(
                chunk,
                key,
                part_number,
                upload_id,
                "application/octet-stream",
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                upload_result = Err(format!("upload part {part_number} failed: {e}"));
                break;
            }
        };

        parts.push(part);
    }

    match upload_result {
        Ok(()) => {
            bucket
                .complete_multipart_upload(key, upload_id, parts)
                .await
                .map_err(|e| format!("complete multipart upload failed: {e}"))?;
            Ok(())
        }
        Err(e) => {
            let _ = bucket.abort_upload(key, upload_id).await;
            Err(e)
        }
    }
}

async fn read_file_chunk(path: &Path, offset: u64, size: usize) -> Result<Vec<u8>, std::io::Error> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut reader = BufReader::with_capacity(size, file);
    let mut buffer = vec![0u8; size];
    let mut total_read = 0;

    while total_read < size {
        let n = reader.read(&mut buffer[total_read..]).await?;
        if n == 0 {
            break;
        }
        total_read += n;
    }

    buffer.truncate(total_read);
    Ok(buffer)
}

async fn upload_payload_simple(
    bucket: &Bucket,
    payload_path: &Path,
    key: &str,
    max_retries: u32,
) -> Result<(), i32> {
    let mut file = tokio::fs::File::open(payload_path).await.map_err(|e| {
        error!(error = %e, "failed to open payload file");
        ExitCode::Retryable as i32
    })?;

    let mut data = Vec::new();
    file.read_to_end(&mut data).await.map_err(|e| {
        error!(error = %e, "failed to read payload file");
        ExitCode::Retryable as i32
    })?;

    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying payload upload");
            tokio::time::sleep(backoff).await;
        }

        match bucket.put_object(key, &data).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap();
    error!(error = %err, "payload upload failed after retries");
    Err(ExitCode::Retryable as i32)
}

fn build_manifest(
    op: &kaniop_backup_core::operation::UploadOperation,
    payload_key: &str,
    checksum_result: &checksum::ChecksumResult,
) -> KanidmBackupManifest {
    KanidmBackupManifest {
        api_version: MANIFEST_API_VERSION_V1.to_string(),
        kind: MANIFEST_KIND.to_string(),
        backup_id: op.backup_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        source: ManifestSource {
            namespace_uid: op.namespace_uid.clone(),
            kanidm_name: op.kanidm_name.clone(),
            kanidm_uid: op.kanidm_uid.clone(),
            domain: op.domain.clone(),
            kanidm_version: op.kanidm_version.clone(),
            image_digest: op.image_digest.clone(),
        },
        backup: ManifestBackup {
            mode: "full".to_string(),
            consistency: op.consistency.clone(),
            reason: op.reason.clone(),
        },
        payload: ManifestPayload {
            key: payload_key.to_string(),
            size_bytes: checksum_result.size_bytes,
            sha256: checksum_result.sha256.clone(),
        },
        encryption: op.encryption_mode.as_ref().map(|mode| ManifestEncryption {
            transport: "tls".to_string(),
            at_rest: mode.clone(),
            key_id: op.encryption_key_id.clone(),
        }),
        compatibility: Some(ManifestCompatibility {
            same_kanidm_version_required: true,
            minimum_manifest_reader: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    }
}

async fn upload_manifest_conditional(
    bucket: &Bucket,
    key: &str,
    manifest_json: &str,
    max_retries: u32,
) -> Result<(), i32> {
    let data = manifest_json.as_bytes();

    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying manifest upload");
            tokio::time::sleep(backoff).await;
        }

        match crate::s3::put_object_conditional(bucket, key, data, "application/json").await {
            Ok(()) => return Ok(()),
            Err(S3Error::ObjectAlreadyExists) => {
                error!(
                    key = %key,
                    "manifest already exists; committed manifests cannot be overwritten"
                );
                return Err(ExitCode::Integrity as i32);
            }
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap();
    error!(error = %err, "manifest upload failed after retries");
    Err(ExitCode::Retryable as i32)
}

fn verify_commit_identity(
    remote: &KanidmBackupManifest,
    local: &KanidmBackupManifest,
) -> Result<(), String> {
    if remote.backup_id != local.backup_id {
        return Err(format!(
            "manifest backup ID mismatch: remote={}, local={}",
            remote.backup_id, local.backup_id
        ));
    }
    if remote.payload.sha256 != local.payload.sha256 {
        return Err(format!(
            "manifest payload checksum mismatch: remote={}, local={}",
            remote.payload.sha256, local.payload.sha256
        ));
    }
    Ok(())
}

async fn verify_commit(
    bucket: &Bucket,
    manifest_key: &str,
    expected_json: &str,
) -> Result<(), i32> {
    let response = bucket.get_object(manifest_key).await.map_err(|e| {
        error!(error = %e, key = %manifest_key, "manifest verification GET failed");
        ExitCode::Integrity as i32
    })?;

    if response.status_code() != 200 {
        error!(code = response.status_code(), key = %manifest_key, "manifest verification returned non-200");
        return Err(ExitCode::Integrity as i32);
    }

    let remote_json = String::from_utf8(response.to_vec()).map_err(|e| {
        error!(error = %e, "manifest is not valid UTF-8");
        ExitCode::Integrity as i32
    })?;

    let remote_manifest: KanidmBackupManifest =
        serde_json::from_str(&remote_json).map_err(|e| {
            error!(error = %e, "remote manifest is not valid JSON");
            ExitCode::Integrity as i32
        })?;

    let local_manifest: KanidmBackupManifest =
        serde_json::from_str(expected_json).map_err(|e| {
            error!(error = %e, "local manifest is not valid JSON");
            ExitCode::Integrity as i32
        })?;

    if let Err(msg) = verify_commit_identity(&remote_manifest, &local_manifest) {
        error!("{msg}");
        return Err(ExitCode::Integrity as i32);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_backup_core::operation::UploadOperation;

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
    fn build_manifest_produces_valid_manifest() {
        let op = test_upload_op();
        let checksum_result = checksum::ChecksumResult {
            sha256: "abc123".to_string(),
            size_bytes: 1024,
        };
        let payload_key = "prod/v1/tenants/ns-uid/clusters/k-uid/backups/019c7c76-f423-7a12-8f41-2bea7588a303/payload/backup.json";
        let manifest = build_manifest(&op, payload_key, &checksum_result);

        assert_eq!(manifest.api_version, MANIFEST_API_VERSION_V1);
        assert_eq!(manifest.kind, MANIFEST_KIND);
        assert_eq!(manifest.backup_id, op.backup_id);
        assert_eq!(manifest.payload.sha256, "abc123");
        assert_eq!(manifest.payload.size_bytes, 1024);
        assert!(manifest.validate().is_ok());
    }

    #[tokio::test]
    async fn read_file_chunk_reads_correct_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"0123456789ABCDEF").unwrap();

        let chunk = read_file_chunk(&path, 4, 8).await.unwrap();
        assert_eq!(chunk, b"456789AB");

        let chunk = read_file_chunk(&path, 0, 4).await.unwrap();
        assert_eq!(chunk, b"0123");

        let chunk = read_file_chunk(&path, 12, 100).await.unwrap();
        assert_eq!(chunk, b"CDEF");
    }

    #[test]
    fn verify_commit_identity_matching_passes() {
        let m = KanidmBackupManifest {
            api_version: MANIFEST_API_VERSION_V1.to_string(),
            kind: MANIFEST_KIND.to_string(),
            backup_id: "id-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            source: kaniop_backup_core::manifest::ManifestSource {
                namespace_uid: "ns".to_string(),
                kanidm_name: "k".to_string(),
                kanidm_uid: "uid".to_string(),
                domain: "d".to_string(),
                kanidm_version: "1.0".to_string(),
                image_digest: None,
            },
            backup: kaniop_backup_core::manifest::ManifestBackup {
                mode: "full".to_string(),
                consistency: "c".to_string(),
                reason: "r".to_string(),
            },
            payload: kaniop_backup_core::manifest::ManifestPayload {
                key: "k".to_string(),
                size_bytes: 100,
                sha256: "abc".to_string(),
            },
            encryption: None,
            compatibility: None,
        };
        assert!(verify_commit_identity(&m, &m).is_ok());
    }

    #[test]
    fn verify_commit_identity_backup_id_mismatch() {
        let m1 = KanidmBackupManifest {
            api_version: MANIFEST_API_VERSION_V1.to_string(),
            kind: MANIFEST_KIND.to_string(),
            backup_id: "id-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            source: kaniop_backup_core::manifest::ManifestSource {
                namespace_uid: "ns".to_string(),
                kanidm_name: "k".to_string(),
                kanidm_uid: "uid".to_string(),
                domain: "d".to_string(),
                kanidm_version: "1.0".to_string(),
                image_digest: None,
            },
            backup: kaniop_backup_core::manifest::ManifestBackup {
                mode: "full".to_string(),
                consistency: "c".to_string(),
                reason: "r".to_string(),
            },
            payload: kaniop_backup_core::manifest::ManifestPayload {
                key: "k".to_string(),
                size_bytes: 100,
                sha256: "abc".to_string(),
            },
            encryption: None,
            compatibility: None,
        };
        let mut m2 = m1.clone();
        m2.backup_id = "id-2".to_string();
        let result = verify_commit_identity(&m1, &m2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("backup ID mismatch"));
    }

    #[test]
    fn verify_commit_identity_checksum_mismatch() {
        let m1 = KanidmBackupManifest {
            api_version: MANIFEST_API_VERSION_V1.to_string(),
            kind: MANIFEST_KIND.to_string(),
            backup_id: "id-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            source: kaniop_backup_core::manifest::ManifestSource {
                namespace_uid: "ns".to_string(),
                kanidm_name: "k".to_string(),
                kanidm_uid: "uid".to_string(),
                domain: "d".to_string(),
                kanidm_version: "1.0".to_string(),
                image_digest: None,
            },
            backup: kaniop_backup_core::manifest::ManifestBackup {
                mode: "full".to_string(),
                consistency: "c".to_string(),
                reason: "r".to_string(),
            },
            payload: kaniop_backup_core::manifest::ManifestPayload {
                key: "k".to_string(),
                size_bytes: 100,
                sha256: "abc".to_string(),
            },
            encryption: None,
            compatibility: None,
        };
        let mut m2 = m1.clone();
        m2.payload.sha256 = "def".to_string();
        let result = verify_commit_identity(&m1, &m2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("checksum mismatch"));
    }

    #[test]
    fn build_manifest_roundtrip_with_commit_verification() {
        let op = test_upload_op();
        let checksum_result = checksum::ChecksumResult {
            sha256: "deadbeef".to_string(),
            size_bytes: 2048,
        };
        let payload_key = "prod/v1/tenants/ns-uid/clusters/k-uid/backups/019c7c76-f423-7a12-8f41-2bea7588a303/payload/backup.json";
        let manifest = build_manifest(&op, payload_key, &checksum_result);

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed = kaniop_backup_core::manifest::parse_manifest(&json).unwrap();

        assert!(verify_commit_identity(&manifest, &parsed).is_ok());
        assert_eq!(parsed.payload.sha256, "deadbeef");
        assert_eq!(parsed.payload.size_bytes, 2048);
    }
}
