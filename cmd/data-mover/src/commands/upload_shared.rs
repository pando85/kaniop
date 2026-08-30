use std::path::Path;
use std::time::Duration;

use kaniop_backup_core::manifest::{
    ClientSideEncryptionMeta, KanidmBackupManifest, MANIFEST_API_VERSION_V1, MANIFEST_KIND,
    ManifestBackup, ManifestCompatibility, ManifestEncryption, ManifestPayload, ManifestSource,
};
use s3::bucket::Bucket;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader};
use tracing::{error, info, warn};

use crate::checksum;
use crate::crypto::{self, EnvelopeKeys};
use crate::s3::{DEFAULT_PART_SIZE, S3Error, SseHeaders};

pub struct ManifestParams<'a> {
    pub backup_id: &'a str,
    pub namespace_uid: &'a str,
    pub kanidm_uid: &'a str,
    pub kanidm_name: &'a str,
    pub domain: &'a str,
    pub kanidm_version: &'a str,
    pub image_digest: Option<&'a str>,
    pub consistency: &'a str,
    pub reason: &'a str,
    pub encryption_mode: Option<&'a str>,
    pub encryption_key_id: Option<&'a str>,
    pub client_side_meta: Option<ClientSideEncryptionMeta>,
}

pub struct UploadEncryptionConfig {
    pub sse: Option<SseHeaders>,
    pub envelope: Option<(EnvelopeKeys, ClientSideEncryptionMeta)>,
}

pub async fn upload_payload_streaming(
    bucket: &Bucket,
    payload_path: &Path,
    key: &str,
    max_retries: u32,
    _max_concurrent_parts: u32,
    enc: &UploadEncryptionConfig,
) -> Result<(), i32> {
    let file_size = tokio::fs::metadata(payload_path)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to get file metadata");
            kaniop_backup_core::result::ExitCode::Retryable as i32
        })?
        .len();

    let part_size = DEFAULT_PART_SIZE;
    let use_multipart = file_size > part_size as u64;

    if !use_multipart {
        return upload_payload_simple(bucket, payload_path, key, max_retries, enc).await;
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

        match upload_multipart_streaming(bucket, payload_path, key, part_size, file_size, enc).await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(error = %e, attempt, "multipart upload attempt failed");
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap_or_else(|| "unknown error".to_string());
    error!(error = %err, "payload multipart upload failed after retries");
    Err(kaniop_backup_core::result::ExitCode::Retryable as i32)
}

async fn upload_multipart_streaming(
    bucket: &Bucket,
    payload_path: &Path,
    key: &str,
    part_size: usize,
    file_size: u64,
    enc: &UploadEncryptionConfig,
) -> Result<(), String> {
    let response = crate::s3::initiate_multipart_upload_with_sse(
        bucket,
        key,
        "application/octet-stream",
        enc.sse.as_ref(),
    )
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
        let mut chunk = match chunk_result {
            Ok(c) => c,
            Err(e) => {
                upload_result = Err(format!("failed to read chunk at offset {offset}: {e}"));
                break;
            }
        };

        if let Some((ref keys, ref _meta)) = enc.envelope {
            let nonce = crypto::derive_nonce(&keys.nonce_salt, (part_number - 1) as u64);
            chunk = match crypto::seal_chunk(&keys.dek, &nonce, &chunk) {
                Ok(ct) => ct,
                Err(e) => {
                    upload_result = Err(format!("encryption of part {part_number} failed: {e}"));
                    break;
                }
            };
        }

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

pub async fn read_file_chunk(
    path: &Path,
    offset: u64,
    size: usize,
) -> Result<Vec<u8>, std::io::Error> {
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
    enc: &UploadEncryptionConfig,
) -> Result<(), i32> {
    let mut file = tokio::fs::File::open(payload_path).await.map_err(|e| {
        error!(error = %e, "failed to open payload file");
        kaniop_backup_core::result::ExitCode::Retryable as i32
    })?;

    let mut data = Vec::new();
    file.read_to_end(&mut data).await.map_err(|e| {
        error!(error = %e, "failed to read payload file");
        kaniop_backup_core::result::ExitCode::Retryable as i32
    })?;

    if let Some((ref keys, ref _meta)) = enc.envelope {
        let nonce = crypto::derive_nonce(&keys.nonce_salt, 0);
        data = crypto::seal_chunk(&keys.dek, &nonce, &data).map_err(|e| {
            error!(error = %e, "client-side encryption failed");
            kaniop_backup_core::result::ExitCode::Retryable as i32
        })?;
    }

    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying payload upload");
            tokio::time::sleep(backoff).await;
        }

        match crate::s3::put_object_with_sse(bucket, key, &data, enc.sse.as_ref()).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap();
    error!(error = %err, "payload upload failed after retries");
    Err(kaniop_backup_core::result::ExitCode::Retryable as i32)
}

pub fn build_manifest(
    params: &ManifestParams,
    payload_key: &str,
    checksum_result: &checksum::ChecksumResult,
) -> KanidmBackupManifest {
    KanidmBackupManifest {
        api_version: MANIFEST_API_VERSION_V1.to_string(),
        kind: MANIFEST_KIND.to_string(),
        backup_id: params.backup_id.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        source: ManifestSource {
            namespace_uid: params.namespace_uid.to_string(),
            kanidm_name: params.kanidm_name.to_string(),
            kanidm_uid: params.kanidm_uid.to_string(),
            domain: params.domain.to_string(),
            kanidm_version: params.kanidm_version.to_string(),
            image_digest: params.image_digest.map(ToString::to_string),
        },
        backup: ManifestBackup {
            mode: "full".to_string(),
            consistency: params.consistency.to_string(),
            reason: params.reason.to_string(),
        },
        payload: ManifestPayload {
            key: payload_key.to_string(),
            size_bytes: checksum_result.size_bytes,
            sha256: checksum_result.sha256.clone(),
        },
        encryption: params.encryption_mode.map(|mode| ManifestEncryption {
            transport: "tls".to_string(),
            at_rest: mode.to_string(),
            key_id: params.encryption_key_id.map(ToString::to_string),
            client_side: params.client_side_meta.clone(),
        }),
        compatibility: Some(ManifestCompatibility {
            same_kanidm_version_required: true,
            minimum_manifest_reader: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    }
}

pub async fn upload_manifest_conditional(
    bucket: &Bucket,
    key: &str,
    manifest_json: &str,
    max_retries: u32,
    sse: Option<&SseHeaders>,
) -> Result<(), i32> {
    let data = manifest_json.as_bytes();

    let mut last_error: Option<String> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying manifest upload");
            tokio::time::sleep(backoff).await;
        }

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::IF_NONE_MATCH,
            http::HeaderValue::from_static("*"),
        );
        if let Some(sse) = sse {
            sse.apply_to_headers(&mut headers);
        }

        let response = bucket
            .put_object_with_content_type_and_headers(key, data, "application/json", Some(headers))
            .await;

        match response {
            Ok(resp) if resp.status_code() == 412 => {
                return Err(S3Error::ObjectAlreadyExists.into());
            }
            Ok(resp) if resp.status_code() >= 400 => {
                last_error = Some(format!(
                    "manifest upload returned status {}",
                    resp.status_code()
                ));
            }
            Ok(_) => return Ok(()),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("PreconditionFailed") || err_str.contains("412") {
                    return Err(S3Error::ObjectAlreadyExists.into());
                }
                last_error = Some(err_str);
            }
        }
    }

    let err = last_error.unwrap();
    error!(error = %err, "manifest upload failed after retries");
    Err(kaniop_backup_core::result::ExitCode::Retryable as i32)
}

pub fn verify_commit_identity(
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

pub async fn verify_commit(
    bucket: &Bucket,
    manifest_key: &str,
    expected_json: &str,
) -> Result<(), i32> {
    let response = bucket.get_object(manifest_key).await.map_err(|e| {
        error!(error = %e, key = %manifest_key, "manifest verification GET failed");
        kaniop_backup_core::result::ExitCode::Integrity as i32
    })?;

    if response.status_code() != 200 {
        error!(code = response.status_code(), key = %manifest_key, "manifest verification returned non-200");
        return Err(kaniop_backup_core::result::ExitCode::Integrity as i32);
    }

    let remote_json = String::from_utf8(response.to_vec()).map_err(|e| {
        error!(error = %e, "manifest is not valid UTF-8");
        kaniop_backup_core::result::ExitCode::Integrity as i32
    })?;

    let remote_manifest: KanidmBackupManifest =
        serde_json::from_str(&remote_json).map_err(|e| {
            error!(error = %e, "remote manifest is not valid JSON");
            kaniop_backup_core::result::ExitCode::Integrity as i32
        })?;

    let local_manifest: KanidmBackupManifest =
        serde_json::from_str(expected_json).map_err(|e| {
            error!(error = %e, "local manifest is not valid JSON");
            kaniop_backup_core::result::ExitCode::Integrity as i32
        })?;

    if let Err(msg) = verify_commit_identity(&remote_manifest, &local_manifest) {
        error!("{msg}");
        return Err(kaniop_backup_core::result::ExitCode::Integrity as i32);
    }

    Ok(())
}

impl From<S3Error> for i32 {
    fn from(err: S3Error) -> Self {
        match err {
            S3Error::ObjectAlreadyExists => kaniop_backup_core::result::ExitCode::Integrity as i32,
            S3Error::MissingCredentials => kaniop_backup_core::result::ExitCode::Retryable as i32,
            _ => kaniop_backup_core::result::ExitCode::Retryable as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest_params() -> ManifestParams<'static> {
        ManifestParams {
            backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303",
            namespace_uid: "ns-uid",
            kanidm_uid: "k-uid",
            kanidm_name: "corp-idm",
            domain: "idm.example.com",
            kanidm_version: "1.10.4",
            image_digest: Some("sha256:abc"),
            consistency: "kanidm-online",
            reason: "scheduled",
            encryption_mode: None,
            encryption_key_id: None,
            client_side_meta: None,
        }
    }

    #[test]
    fn build_manifest_produces_valid_manifest() {
        let params = test_manifest_params();
        let checksum_result = checksum::ChecksumResult {
            sha256: "abc123".to_string(),
            size_bytes: 1024,
        };
        let payload_key = "prod/v1/tenants/ns-uid/clusters/k-uid/backups/019c7c76-f423-7a12-8f41-2bea7588a303/payload/backup.json";
        let manifest = build_manifest(&params, payload_key, &checksum_result);

        assert_eq!(manifest.api_version, MANIFEST_API_VERSION_V1);
        assert_eq!(manifest.kind, MANIFEST_KIND);
        assert_eq!(manifest.backup_id, params.backup_id);
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
        let params = test_manifest_params();
        let checksum_result = checksum::ChecksumResult {
            sha256: "deadbeef".to_string(),
            size_bytes: 2048,
        };
        let payload_key = "prod/v1/tenants/ns-uid/clusters/k-uid/backups/019c7c76-f423-7a12-8f41-2bea7588a303/payload/backup.json";
        let manifest = build_manifest(&params, payload_key, &checksum_result);

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed = kaniop_backup_core::manifest::parse_manifest(&json).unwrap();

        assert!(verify_commit_identity(&manifest, &parsed).is_ok());
        assert_eq!(parsed.payload.sha256, "deadbeef");
        assert_eq!(parsed.payload.size_bytes, 2048);
    }
}
