use std::path::Path;
use std::time::Duration;

use kaniop_backup_core::manifest::{KanidmBackupManifest, parse_manifest};
use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use s3::bucket::Bucket;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::checksum;
use crate::crypto;
use crate::s3::{S3Config, create_bucket};

use super::{load_operation, write_result};

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Download(op) => op,
        _ => {
            error!("expected download operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();

    info!(
        manifest_key = %op.manifest_key,
        backup_id = %op.expected_backup_id,
        "starting download"
    );

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

    let manifest = download_and_verify_manifest(&bucket, &op.manifest_key, op).await?;

    info!(
        backup_id = %manifest.backup_id,
        payload_key = %manifest.payload.key,
        "manifest verified"
    );

    download_and_verify_payload_streaming(&bucket, &manifest, &op.output_path, op.max_retries)
        .await?;

    info!(output = %op.output_path, "payload downloaded and verified");

    let mut result = ResultDocument::success("download");
    result.backup_id = Some(manifest.backup_id.clone());
    result.manifest_key = Some(op.manifest_key.clone());
    result.payload_key = Some(manifest.payload.key.clone());
    result.payload_sha256 = Some(manifest.payload.sha256.clone());
    result.payload_size_bytes = Some(manifest.payload.size_bytes);

    write_result(&result_path, &result).await?;

    info!(backup_id = %manifest.backup_id, "download completed successfully");
    Ok(())
}

fn verify_manifest_identity(
    manifest: &KanidmBackupManifest,
    expected_backup_id: &str,
    expected_kanidm_uid: &str,
    expected_domain: &str,
) -> Result<(), String> {
    if manifest.backup_id != expected_backup_id {
        return Err(format!(
            "manifest backup ID mismatch: expected {expected_backup_id}, got {}",
            manifest.backup_id
        ));
    }
    if manifest.source.kanidm_uid != expected_kanidm_uid {
        return Err(format!(
            "manifest kanidm UID mismatch: expected {expected_kanidm_uid}, got {}",
            manifest.source.kanidm_uid
        ));
    }
    if expected_domain != "*" && manifest.source.domain != expected_domain {
        return Err(format!(
            "manifest domain mismatch: expected {expected_domain}, got {}",
            manifest.source.domain
        ));
    }
    Ok(())
}

fn verify_payload_key_confinement(
    manifest: &KanidmBackupManifest,
    bucket: &str,
    prefix: &str,
) -> Result<(), String> {
    let repo_path =
        RepositoryPath::new(bucket, prefix).map_err(|e| format!("invalid repository path: {e}"))?;
    if !repo_path.contains_key(&manifest.payload.key) {
        return Err(format!(
            "payload key '{}' escapes repository prefix",
            manifest.payload.key
        ));
    }
    Ok(())
}

async fn download_and_verify_manifest(
    bucket: &Bucket,
    manifest_key: &str,
    op: &kaniop_backup_core::operation::DownloadOperation,
) -> Result<KanidmBackupManifest, i32> {
    let response = bucket.get_object(manifest_key).await.map_err(|e| {
        error!(error = %e, key = %manifest_key, "failed to download manifest");
        ExitCode::Retryable as i32
    })?;

    if response.status_code() != 200 {
        error!(code = response.status_code(), key = %manifest_key, "manifest download returned non-200");
        return Err(ExitCode::Retryable as i32);
    }

    let manifest_json = String::from_utf8(response.to_vec()).map_err(|e| {
        error!(error = %e, "manifest is not valid UTF-8");
        ExitCode::Integrity as i32
    })?;

    let manifest = parse_manifest(&manifest_json).map_err(|e| {
        error!(error = %e, "manifest validation failed");
        ExitCode::Integrity as i32
    })?;

    if let Err(msg) = verify_manifest_identity(
        &manifest,
        &op.expected_backup_id,
        &op.expected_kanidm_uid,
        &op.expected_domain,
    ) {
        error!("{msg}");
        return Err(ExitCode::Integrity as i32);
    }

    if let Err(msg) = verify_payload_key_confinement(&manifest, &op.bucket, &op.prefix) {
        error!("{msg}");
        return Err(ExitCode::Integrity as i32);
    }

    Ok(manifest)
}

async fn download_and_verify_payload_streaming(
    bucket: &Bucket,
    manifest: &KanidmBackupManifest,
    output_path: &str,
    max_retries: u32,
) -> Result<(), i32> {
    let payload_key = &manifest.payload.key;
    let expected_sha256 = &manifest.payload.sha256;
    let expected_size = manifest.payload.size_bytes;

    if let Some(parent) = Path::new(output_path).parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            error!(error = %e, "failed to create output directory");
            ExitCode::Retryable as i32
        })?;
    }

    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_secs(2u64.pow(attempt).min(60));
            info!(attempt, ?backoff, "retrying payload download");
            tokio::time::sleep(backoff).await;
        }

        match download_payload_to_file(bucket, payload_key, output_path, manifest).await {
            Ok(()) => {
                let actual_checksum = checksum::compute_sha256(Path::new(output_path))
                    .await
                    .map_err(|e| {
                        error!(error = %e, "failed to compute downloaded payload checksum");
                        ExitCode::Retryable as i32
                    })?;

                match checksum::verify_checksum(
                    &actual_checksum.sha256,
                    expected_sha256,
                    actual_checksum.size_bytes,
                    expected_size,
                ) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, attempt, "payload integrity check failed");
                        last_error = Some(e.to_string());
                        continue;
                    }
                }
            }
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    let err = last_error.unwrap_or_default();
    error!(error = %err, "payload download failed after retries");
    Err(ExitCode::Retryable as i32)
}

async fn download_payload_to_file(
    bucket: &Bucket,
    payload_key: &str,
    output_path: &str,
    manifest: &KanidmBackupManifest,
) -> Result<(), String> {
    let mut file = tokio::fs::File::create(output_path)
        .await
        .map_err(|e| format!("failed to create output file: {e}"))?;

    let response = bucket
        .get_object(payload_key)
        .await
        .map_err(|e| format!("failed to download payload: {e}"))?;

    if response.status_code() != 200 {
        return Err(format!(
            "payload download returned status {}",
            response.status_code()
        ));
    }

    let raw_data = response.to_vec();

    let output_data = if let Some(enc) = &manifest.encryption {
        if let Some(ref client_side) = enc.client_side {
            let (keys, _meta) = crypto::load_envelope_for_download(client_side).map_err(|e| {
                format!(
                    "client-side decryption setup failed (KEK fingerprint {}): {e}",
                    client_side.kek_fingerprint
                )
            })?;
            let chunk_size = client_side.chunk_size_bytes as usize;
            let total_chunks = raw_data.len().div_ceil(chunk_size + crypto::TAG_SIZE);
            let mut plaintext = Vec::with_capacity(raw_data.len());
            let mut offset = 0;
            for chunk_idx in 0..total_chunks {
                let nonce = crypto::derive_nonce(&keys.nonce_salt, chunk_idx as u64);
                let remaining = raw_data.len() - offset;
                let ct_len = std::cmp::min(chunk_size + crypto::TAG_SIZE, remaining);
                let ciphertext = &raw_data[offset..offset + ct_len];
                let chunk_plain = crypto::open_chunk(&keys.dek, &nonce, ciphertext)
                    .map_err(|e| format!("decryption of chunk {chunk_idx} failed: {e}"))?;
                plaintext.extend_from_slice(&chunk_plain);
                offset += ct_len;
            }
            plaintext
        } else {
            raw_data
        }
    } else {
        raw_data
    };

    file.write_all(&output_data)
        .await
        .map_err(|e| format!("failed to write payload: {e}"))?;

    file.flush()
        .await
        .map_err(|e| format!("failed to flush output: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_backup_core::manifest::{ManifestBackup, ManifestPayload, ManifestSource};

    fn test_manifest(backup_id: &str, kanidm_uid: &str, domain: &str) -> KanidmBackupManifest {
        KanidmBackupManifest {
            api_version: "backup.kaniop.rs/v1alpha1".to_string(),
            kind: "KanidmBackupManifest".to_string(),
            backup_id: backup_id.to_string(),
            created_at: "2026-08-18T02:03:41Z".to_string(),
            source: ManifestSource {
                namespace_uid: "ns-uid".to_string(),
                kanidm_name: "corp-idm".to_string(),
                kanidm_uid: kanidm_uid.to_string(),
                domain: domain.to_string(),
                kanidm_version: "1.10.4".to_string(),
                image_digest: None,
            },
            backup: ManifestBackup {
                mode: "full".to_string(),
                consistency: "kanidm-offline".to_string(),
                reason: "scheduled".to_string(),
            },
            payload: ManifestPayload {
                key: "prod/v1/tenants/ns-uid/clusters/k-uid/backups/b1/payload/data.gz".to_string(),
                size_bytes: 1024,
                sha256: "abc123".to_string(),
            },
            encryption: None,
            compatibility: None,
        }
    }

    #[test]
    fn verify_manifest_identity_matching_passes() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        assert!(verify_manifest_identity(&m, "id-1", "k-uid", "idm.example.com").is_ok());
    }

    #[test]
    fn verify_manifest_identity_backup_id_mismatch() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        let result = verify_manifest_identity(&m, "id-2", "k-uid", "idm.example.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("backup ID mismatch"));
    }

    #[test]
    fn verify_manifest_identity_kanidm_uid_mismatch() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        let result = verify_manifest_identity(&m, "id-1", "wrong-uid", "idm.example.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("kanidm UID mismatch"));
    }

    #[test]
    fn verify_manifest_identity_domain_wildcard() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        assert!(verify_manifest_identity(&m, "id-1", "k-uid", "*").is_ok());
    }

    #[test]
    fn verify_manifest_identity_domain_mismatch() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        let result = verify_manifest_identity(&m, "id-1", "k-uid", "wrong.example.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("domain mismatch"));
    }

    #[test]
    fn verify_payload_key_confinement_valid_key() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        assert!(verify_payload_key_confinement(&m, "bucket", "prod").is_ok());
    }

    #[test]
    fn verify_payload_key_confinement_escaping_key_rejected() {
        let mut m = test_manifest("id-1", "k-uid", "idm.example.com");
        m.payload.key = "other-prefix/v1/tenants/ns/clusters/k/backups/b/payload/data".to_string();
        let result = verify_payload_key_confinement(&m, "bucket", "prod");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes repository prefix"));
    }

    #[test]
    fn verify_payload_key_confinement_traversal_rejected() {
        let mut m = test_manifest("id-1", "k-uid", "idm.example.com");
        m.payload.key = "prod/v1/tenants/../clusters/k/backups/b/payload/data".to_string();
        let result = verify_payload_key_confinement(&m, "bucket", "prod");
        assert!(result.is_err());
    }
}
