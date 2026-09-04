use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use flate2::read::GzDecoder;
use kaniop_backup_core::operation::{CheckFormat, OperationSpec};
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use serde_json::Value;
use tracing::{error, info};

use crate::checksum::compute_sha256;

use super::{load_operation, write_result};

const KANIDM_DOMAIN_INFO_UUID: &str = "00000000-0000-0000-0000-ffffff000025";

fn extract_kanidm_domain(backup: &Value) -> Option<String> {
    let entries = backup
        .get("entries")
        .and_then(Value::as_array)
        .or_else(|| backup.as_array())?;

    entries.iter().find_map(|entry| {
        let attrs = entry.get("ent")?.get("V3")?.get("attrs")?.as_object()?;
        let is_domain_info = attrs
            .get("uuid")?
            .get("UU")?
            .as_array()?
            .iter()
            .any(|uuid| uuid.as_str() == Some(KANIDM_DOMAIN_INFO_UUID));
        if !is_domain_info {
            return None;
        }

        attrs
            .get("domain_name")?
            .get("N8")?
            .as_array()?
            .first()?
            .as_str()
            .map(str::to_string)
    })
}

fn validate_kanidm_json_gzip(
    path: &Path,
    expected_domain: Option<&str>,
) -> Result<Option<String>, (ExitCode, &'static str, String)> {
    let file = File::open(path).map_err(|e| {
        (
            ExitCode::Retryable,
            "FILE_OPEN_ERROR",
            format!("failed to open file for validation: {e}"),
        )
    })?;
    let reader = BufReader::new(file);
    let decoder = GzDecoder::new(reader);
    let mut buf_reader = BufReader::new(decoder);
    let mut buf = Vec::new();
    buf_reader.read_to_end(&mut buf).map_err(|e| {
        (
            ExitCode::Integrity,
            "GZIP_INTEGRITY_ERROR",
            format!("gzip integrity check failed: {e}"),
        )
    })?;
    let backup: Value = serde_json::from_slice(&buf).map_err(|e| {
        (
            ExitCode::Integrity,
            "INVALID_JSON_PAYLOAD",
            format!("decompressed payload is not valid JSON: {e}"),
        )
    })?;

    let domain = extract_kanidm_domain(&backup);
    if let Some(expected) = expected_domain {
        let actual = domain.as_deref().ok_or_else(|| {
            (
                ExitCode::Integrity,
                "KANIDM_DOMAIN_NOT_FOUND",
                "Kanidm backup does not contain the built-in domain information entry".to_string(),
            )
        })?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err((
                ExitCode::InvalidInput,
                "DOMAIN_MISMATCH",
                format!("backup domain '{actual}' does not match target domain '{expected}'"),
            ));
        }
    }

    Ok(domain)
}

pub async fn run(operation_doc_path: &str) -> Result<(), i32> {
    let doc = load_operation(operation_doc_path).await?;
    let op = match &doc.spec {
        OperationSpec::Check(op) => op,
        _ => {
            error!("expected check operation");
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    let result_path = op.result_path.clone();
    let target = Path::new(&op.path);

    info!(path = %op.path, "checking local backup file");

    let metadata = match tokio::fs::metadata(target).await {
        Ok(metadata) => metadata,
        Err(e) => {
            error!(path = %op.path, error = %e, "backup file not found");
            let result = ResultDocument::failure(
                "check",
                ExitCode::InvalidInput,
                "FILE_NOT_FOUND",
                &format!("backup file not found at {}", op.path),
            );
            write_result(&result_path, &result).await?;
            return Err(ExitCode::InvalidInput as i32);
        }
    };

    if !metadata.is_file() {
        error!(path = %op.path, "backup path is not a regular file");
        let result = ResultDocument::failure(
            "check",
            ExitCode::InvalidInput,
            "NOT_A_FILE",
            &format!("backup path is not a regular file: {}", op.path),
        );
        write_result(&result_path, &result).await?;
        return Err(ExitCode::InvalidInput as i32);
    }

    if metadata.len() == 0 {
        error!(path = %op.path, "backup file is empty");
        let result = ResultDocument::failure(
            "check",
            ExitCode::Integrity,
            "EMPTY_FILE",
            &format!("backup file is empty: {}", op.path),
        );
        write_result(&result_path, &result).await?;
        return Err(ExitCode::Integrity as i32);
    }

    if op.format == Some(CheckFormat::KanidmJsonGzip) {
        info!(path = %op.path, "validating gzip integrity and Kanidm JSON payload");
        match validate_kanidm_json_gzip(target, op.expected_domain.as_deref()) {
            Ok(domain) => {
                if let Some(domain) = domain {
                    info!(path = %op.path, %domain, "Kanidm backup domain validated");
                }
            }
            Err((exit_code, code, message)) => {
                error!(path = %op.path, code = code, %message, "backup content validation failed");
                let result = ResultDocument::failure("check", exit_code, code, &message);
                write_result(&result_path, &result).await?;
                return Err(exit_code.as_i32());
            }
        }
        info!(path = %op.path, "gzip and Kanidm JSON validation passed");
    }

    let checksum = compute_sha256(target).await.map_err(|e| {
        error!(path = %op.path, error = %e, "failed to checksum backup file");
        ExitCode::Retryable as i32
    })?;

    info!(
        path = %op.path,
        size = checksum.size_bytes,
        sha256 = %checksum.sha256,
        "backup file check passed"
    );

    let mut result = ResultDocument::success("check");
    result.payload_sha256 = Some(checksum.sha256);
    result.payload_size_bytes = Some(checksum.size_bytes);
    write_result(&result_path, &result).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tempfile::NamedTempFile;

    use super::*;

    fn write_temp_file(contents: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents).unwrap();
        f.flush().unwrap();
        f
    }

    fn gzip_json(value: &Value) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(serde_json::to_vec(value).unwrap().as_slice())
            .unwrap();
        encoder.finish().unwrap()
    }

    fn make_valid_gzip_json() -> Vec<u8> {
        gzip_json(&serde_json::json!({"version":"1.10.4","entries":[]}))
    }

    fn make_kanidm_backup(domain: &str) -> Vec<u8> {
        gzip_json(&serde_json::json!({
            "version": "1.11.1",
            "entries": [{
                "ent": {
                    "V3": {
                        "changestate": {},
                        "attrs": {
                            "uuid": {"UU": [KANIDM_DOMAIN_INFO_UUID]},
                            "domain_name": {"N8": [domain]}
                        }
                    }
                }
            }]
        }))
    }

    #[test]
    fn valid_gzip_json_passes_without_semantic_constraint() {
        let data = make_valid_gzip_json();
        let f = write_temp_file(&data);
        assert!(validate_kanidm_json_gzip(f.path(), None).is_ok());
    }

    #[test]
    fn kanidm_domain_is_extracted_and_matches() {
        let data = make_kanidm_backup("idm.example.com");
        let f = write_temp_file(&data);
        let domain = validate_kanidm_json_gzip(f.path(), Some("idm.example.com")).unwrap();
        assert_eq!(domain.as_deref(), Some("idm.example.com"));
    }

    #[test]
    fn domain_match_is_case_insensitive() {
        let data = make_kanidm_backup("IDM.EXAMPLE.COM");
        let f = write_temp_file(&data);
        assert!(validate_kanidm_json_gzip(f.path(), Some("idm.example.com")).is_ok());
    }

    #[test]
    fn mismatched_domain_is_rejected() {
        let data = make_kanidm_backup("old.example.com");
        let f = write_temp_file(&data);
        let err = validate_kanidm_json_gzip(f.path(), Some("idm.example.com")).unwrap_err();
        assert_eq!(err.0, ExitCode::InvalidInput);
        assert_eq!(err.1, "DOMAIN_MISMATCH");
        assert!(err.2.contains("old.example.com"));
        assert!(err.2.contains("idm.example.com"));
    }

    #[test]
    fn missing_domain_is_rejected_when_expected() {
        let data = make_valid_gzip_json();
        let f = write_temp_file(&data);
        let err = validate_kanidm_json_gzip(f.path(), Some("idm.example.com")).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "KANIDM_DOMAIN_NOT_FOUND");
    }

    #[test]
    fn plain_garbage_fails() {
        let f = write_temp_file(b"this is not gzip at all, just random bytes");
        let err = validate_kanidm_json_gzip(f.path(), None).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "GZIP_INTEGRITY_ERROR");
    }

    #[test]
    fn truncated_gzip_fails() {
        let data = make_valid_gzip_json();
        let truncated = &data[..data.len() / 2];
        let f = write_temp_file(truncated);
        let err = validate_kanidm_json_gzip(f.path(), None).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "GZIP_INTEGRITY_ERROR");
    }

    #[test]
    fn valid_gzip_non_json_fails() {
        let not_json = b"this is plain text, not JSON";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(not_json).unwrap();
        let data = encoder.finish().unwrap();
        let f = write_temp_file(&data);
        let err = validate_kanidm_json_gzip(f.path(), None).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "INVALID_JSON_PAYLOAD");
    }
}
