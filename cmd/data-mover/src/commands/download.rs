use std::path::Path;
use std::time::Duration;

use kaniop_backup_core::manifest::{KanidmBackupManifest, parse_manifest};
use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, MANIFEST_NOT_FOUND, ResultDocument};
use s3::bucket::Bucket;
use s3::error::S3Error;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::checksum;
use crate::crypto;
use crate::s3::{S3Config, create_bucket};

use super::{load_operation, write_result};

/// Error code reported when the manifest object exists but could not be fetched, or the fetch
/// outcome is not definitive. Unlike [`MANIFEST_NOT_FOUND`] the condition may clear, so the exit
/// code stays retryable.
const MANIFEST_FETCH_ERROR: &str = "MANIFEST_FETCH_ERROR";

/// S3 error code carried by an XML error document, e.g. `NoSuchKey` in
/// `<Error><Code>NoSuchKey</Code><Key>…</Key></Error>`.
fn s3_error_code(body: &str) -> Option<&str> {
    const OPEN_TAG: &str = "<Code>";
    const CLOSE_TAG: &str = "</Code>";
    let start = body.find(OPEN_TAG)? + OPEN_TAG.len();
    let end = start + body[start..].find(CLOSE_TAG)?;
    Some(body[start..end].trim())
}

/// An object is unambiguously absent only when the provider answers `404` with an explicit
/// `NoSuchKey` code, which is what AWS S3 and MinIO return for a deleted key. Discovery-owned
/// records are deleted on this signal, so anything less definitive must stay a retryable fetch
/// failure: `NoSuchBucket` reports a misconfigured repository, a misrouted endpoint or a
/// path-style versus virtual-host mismatch is also a `404`, and S3 answers `404 NoSuchKey` to
/// `GetObject` when `s3:ListBucket` is missing, which would otherwise turn a permissions
/// regression into data loss.
fn manifest_absent(status: u16, body: &str) -> bool {
    status == 404 && s3_error_code(body) == Some("NoSuchKey")
}

/// Classifies a `get_object` failure for the manifest. Pure so it can be unit tested without an
/// object store.
fn classify_manifest_fetch_error(
    error: &S3Error,
    manifest_key: &str,
) -> (ExitCode, &'static str, String) {
    match error {
        S3Error::HttpFailWithBody(status, body) if manifest_absent(*status, body) => (
            ExitCode::InvalidInput,
            MANIFEST_NOT_FOUND,
            format!("manifest object not found at {manifest_key} (HTTP {status})"),
        ),
        _ => (
            ExitCode::Retryable,
            MANIFEST_FETCH_ERROR,
            format!("failed to download manifest {manifest_key}: {error}"),
        ),
    }
}

/// Classifies a manifest response that reached the object store but did not return `200 OK`.
/// Absence requires the same explicit `NoSuchKey` document as a failed request, so a bare or
/// unrecognized `404` body stays retryable instead of discarding catalogue entries.
fn classify_manifest_response_status(
    status: u16,
    body: &str,
    manifest_key: &str,
) -> (ExitCode, &'static str, String) {
    if manifest_absent(status, body) {
        return (
            ExitCode::InvalidInput,
            MANIFEST_NOT_FOUND,
            format!("manifest object not found at {manifest_key} (HTTP {status})"),
        );
    }
    (
        ExitCode::Retryable,
        MANIFEST_FETCH_ERROR,
        format!("manifest download returned HTTP {status} for {manifest_key}"),
    )
}

/// Writes a manifest failure result document and returns the process exit code. The document is
/// the operator's only way to tell the outcomes apart, so the classified exit code is reported even
/// when writing it fails.
async fn report_manifest_failure(
    result_path: &str,
    manifest_key: &str,
    failure: (ExitCode, &'static str, String),
) -> i32 {
    let (exit_code, code, message) = failure;
    error!(code = code, key = %manifest_key, %message, "failed to download manifest");
    let result = ResultDocument::failure("download", exit_code, code, &message);
    if let Err(error) = write_result(result_path, &result).await {
        error!(error = %error, "failed to write manifest failure result document");
    }
    exit_code.as_i32()
}

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

    if op.manifest_only {
        let result = build_download_result(&manifest, &op.manifest_key);
        write_result(&result_path, &result).await?;
        info!(backup_id = %manifest.backup_id, "manifest-only validation completed");
        return Ok(());
    }

    download_and_verify_payload_streaming(&bucket, &manifest, &op.output_path, op.max_retries)
        .await?;

    info!(output = %op.output_path, "payload downloaded and verified");

    let result = build_download_result(&manifest, &op.manifest_key);
    write_result(&result_path, &result).await?;

    info!(backup_id = %manifest.backup_id, "download completed successfully");
    Ok(())
}

fn build_download_result(manifest: &KanidmBackupManifest, manifest_key: &str) -> ResultDocument {
    let mut result = ResultDocument::success("download");
    result.backup_id = Some(manifest.backup_id.clone());
    result.manifest_key = Some(manifest_key.to_string());
    result.payload_key = Some(manifest.payload.key.clone());
    result.payload_sha256 = Some(manifest.payload.sha256.clone());
    result.payload_size_bytes = Some(manifest.payload.size_bytes);
    result.created_at = Some(manifest.created_at.clone());
    result.consistency = Some(manifest.backup.consistency.clone());
    result.reason = Some(manifest.backup.reason.clone());
    result.kanidm_version = Some(manifest.source.kanidm_version.clone());
    result.image_digest = manifest.source.image_digest.clone();
    result
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
    let response = match bucket.get_object(manifest_key).await {
        Ok(response) => response,
        Err(e) => {
            let failure = classify_manifest_fetch_error(&e, manifest_key);
            return Err(report_manifest_failure(&op.result_path, manifest_key, failure).await);
        }
    };

    if response.status_code() != 200 {
        let status = response.status_code();
        let body = String::from_utf8_lossy(response.as_slice());
        let failure = classify_manifest_response_status(status, &body, manifest_key);
        return Err(report_manifest_failure(&op.result_path, manifest_key, failure).await);
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
    use std::io;

    use tempfile::tempdir;

    use kaniop_backup_core::result::parse_result_document;

    use super::*;
    use kaniop_backup_core::manifest::{ManifestBackup, ManifestPayload, ManifestSource};

    const TEST_MANIFEST_KEY: &str = "prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json";

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
                image_digest: Some("sha256:abc".to_string()),
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

    #[test]
    fn build_download_result_populates_all_fields() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        let result = build_download_result(&m, "manifests/m1.json");
        assert!(result.success);
        assert_eq!(result.backup_id.as_deref(), Some("id-1"));
        assert_eq!(result.manifest_key.as_deref(), Some("manifests/m1.json"));
        assert_eq!(result.payload_key.as_deref(), Some(m.payload.key.as_str()));
        assert_eq!(result.payload_sha256.as_deref(), Some("abc123"));
        assert_eq!(result.payload_size_bytes, Some(1024));
        assert_eq!(result.created_at.as_deref(), Some("2026-08-18T02:03:41Z"));
        assert_eq!(result.consistency.as_deref(), Some("kanidm-offline"));
        assert_eq!(result.reason.as_deref(), Some("scheduled"));
        assert_eq!(result.kanidm_version.as_deref(), Some("1.10.4"));
        assert_eq!(result.image_digest.as_deref(), Some("sha256:abc"));
    }

    #[test]
    fn build_download_result_does_not_require_payload_file() {
        let m = test_manifest("id-1", "k-uid", "idm.example.com");
        let result = build_download_result(&m, "manifests/m1.json");
        assert!(result.success);
        assert_eq!(result.operation, "download");
    }

    /// The document AWS S3 and MinIO return for a deleted key, including the `<Key>` element the
    /// real providers add after the code.
    const NO_SUCH_KEY_BODY: &str = "<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json</Key></Error>";
    /// A missing bucket answers `404` too, but says nothing about the key.
    const NO_SUCH_BUCKET_BODY: &str = "<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>";

    #[test]
    fn manifest_absent_requires_http_404_and_no_such_key() {
        assert!(manifest_absent(404, NO_SUCH_KEY_BODY));
        assert!(
            !manifest_absent(404, NO_SUCH_BUCKET_BODY),
            "a missing bucket must not be reported as an absent key"
        );
        assert!(
            !manifest_absent(404, ""),
            "a bare 404 without an error document is ambiguous"
        );
        assert!(
            !manifest_absent(404, "<Error><Code>NotFound</Code></Error>"),
            "NotFound without NoSuchKey is ambiguous"
        );
        assert!(!manifest_absent(405, NO_SUCH_KEY_BODY));
        assert!(!manifest_absent(403, NO_SUCH_KEY_BODY));
    }

    #[test]
    fn error_code_is_read_from_the_s3_error_document() {
        assert_eq!(s3_error_code(NO_SUCH_KEY_BODY), Some("NoSuchKey"));
        assert_eq!(s3_error_code(NO_SUCH_BUCKET_BODY), Some("NoSuchBucket"));
        assert_eq!(
            s3_error_code("<Error><Code>NoSuchKey</Code></Error>"),
            Some("NoSuchKey")
        );
        assert_eq!(s3_error_code("Internal Server Error"), None);
        assert_eq!(s3_error_code("<Error><Code></Code></Error>"), Some(""));
    }

    #[test]
    fn absent_manifest_http_404_is_classified_as_manifest_not_found() {
        let error = S3Error::HttpFailWithBody(404, NO_SUCH_KEY_BODY.to_string());
        let (exit_code, code, message) = classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::InvalidInput);
        assert_eq!(code, MANIFEST_NOT_FOUND);
        assert!(message.contains(TEST_MANIFEST_KEY));
        assert!(message.contains("404"));
    }

    #[test]
    fn no_such_bucket_http_404_stays_retryable() {
        let error = S3Error::HttpFailWithBody(404, NO_SUCH_BUCKET_BODY.to_string());
        let (exit_code, code, _) = classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::Retryable);
        assert_eq!(code, MANIFEST_FETCH_ERROR);
    }

    #[test]
    fn ambiguous_http_404_bodies_stay_retryable() {
        for body in [
            String::new(),
            "<Error><Code>NotFound</Code></Error>".to_string(),
            "<html><body>NoSuchKey</body></html>".to_string(),
            "<Error><Message>missing</Message></Error>".to_string(),
        ] {
            let error = S3Error::HttpFailWithBody(404, body.clone());
            let (exit_code, code, _) = classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
            assert_eq!(exit_code, ExitCode::Retryable, "body {body:?}");
            assert_eq!(code, MANIFEST_FETCH_ERROR, "body {body:?}");
        }
    }

    #[test]
    fn no_such_key_document_without_http_404_stays_retryable() {
        let error = S3Error::HttpFailWithBody(405, NO_SUCH_KEY_BODY.to_string());
        let (exit_code, code, _) = classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::Retryable);
        assert_eq!(code, MANIFEST_FETCH_ERROR);
    }

    #[test]
    fn denied_and_server_side_manifest_errors_stay_retryable() {
        for (status, body) in [
            (403, "<Error><Code>AccessDenied</Code></Error>"),
            (500, "Internal Server Error"),
            (503, "<Error><Code>ServiceUnavailable</Code></Error>"),
        ] {
            let error = S3Error::HttpFailWithBody(status, body.to_string());
            let (exit_code, code, message) =
                classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
            assert_eq!(exit_code, ExitCode::Retryable, "status {status}");
            assert_eq!(code, MANIFEST_FETCH_ERROR, "status {status}");
            assert!(message.contains(TEST_MANIFEST_KEY), "status {status}");
        }
    }

    #[test]
    fn transport_manifest_error_stays_retryable() {
        let error = S3Error::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "connection timed out",
        ));
        let (exit_code, code, message) = classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::Retryable);
        assert_eq!(code, MANIFEST_FETCH_ERROR);
        assert!(message.contains(TEST_MANIFEST_KEY));
        assert!(message.contains("connection timed out"));
    }

    #[test]
    fn manifest_response_status_classifies_absent_and_failed_responses() {
        let (exit_code, code, message) =
            classify_manifest_response_status(404, NO_SUCH_KEY_BODY, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::InvalidInput);
        assert_eq!(code, MANIFEST_NOT_FOUND);
        assert!(message.contains(TEST_MANIFEST_KEY));

        let (exit_code, code, message) =
            classify_manifest_response_status(404, "", TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::Retryable);
        assert_eq!(code, MANIFEST_FETCH_ERROR);
        assert!(message.contains(TEST_MANIFEST_KEY));

        let (exit_code, code, message) =
            classify_manifest_response_status(503, NO_SUCH_KEY_BODY, TEST_MANIFEST_KEY);
        assert_eq!(exit_code, ExitCode::Retryable);
        assert_eq!(code, MANIFEST_FETCH_ERROR);
        assert!(message.contains(TEST_MANIFEST_KEY));
    }

    /// Runs the failure reporting end to end and returns the exit code together with the result
    /// document the Job wrapper hands to the operator.
    async fn report_and_read_failure(error: S3Error) -> (i32, ResultDocument) {
        let dir = tempdir().unwrap();
        let result_path = dir.path().join("result.json");
        let exit_code = report_manifest_failure(
            result_path.to_str().unwrap(),
            TEST_MANIFEST_KEY,
            classify_manifest_fetch_error(&error, TEST_MANIFEST_KEY),
        )
        .await;
        let json = tokio::fs::read_to_string(&result_path).await.unwrap();
        (exit_code, parse_result_document(&json).unwrap())
    }

    #[tokio::test]
    async fn absent_manifest_reports_failure_document() {
        let (exit_code, doc) = report_and_read_failure(S3Error::HttpFailWithBody(
            404,
            "<Error><Code>NoSuchKey</Code></Error>".to_string(),
        ))
        .await;
        assert_eq!(exit_code, ExitCode::InvalidInput as i32);
        assert_eq!(doc.operation, "download");
        assert!(!doc.success);
        assert_eq!(doc.exit_code, ExitCode::InvalidInput);
        let failure = doc.error.unwrap();
        assert_eq!(failure.code, MANIFEST_NOT_FOUND);
        assert!(failure.message.contains(TEST_MANIFEST_KEY));
    }

    #[tokio::test]
    async fn unreachable_manifest_reports_retryable_failure_document() {
        let (exit_code, doc) = report_and_read_failure(S3Error::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        )))
        .await;
        assert_eq!(exit_code, ExitCode::Retryable as i32);
        assert_eq!(doc.exit_code, ExitCode::Retryable);
        assert_eq!(doc.error.unwrap().code, MANIFEST_FETCH_ERROR);
    }
}
