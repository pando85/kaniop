use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use flate2::read::GzDecoder;
use kaniop_backup_core::operation::{CheckFormat, OperationSpec};
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use tracing::{error, info};

use crate::checksum::compute_sha256;

use super::{load_operation, write_result};

fn validate_kanidm_json_gzip(path: &Path) -> Result<(), (ExitCode, &'static str, String)> {
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
    let _: serde_json::Value = serde_json::from_slice(&buf).map_err(|e| {
        (
            ExitCode::Integrity,
            "INVALID_JSON_PAYLOAD",
            format!("decompressed payload is not valid JSON: {e}"),
        )
    })?;
    Ok(())
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
        info!(path = %op.path, "validating gzip integrity and JSON payload");
        if let Err((exit_code, code, message)) = validate_kanidm_json_gzip(target) {
            error!(path = %op.path, code = code, %message, "backup content validation failed");
            let result = ResultDocument::failure("check", exit_code, code, &message);
            write_result(&result_path, &result).await?;
            return Err(exit_code.as_i32());
        }
        info!(path = %op.path, "gzip and JSON validation passed");
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

    fn make_valid_gzip_json() -> Vec<u8> {
        let json = br#"{"version":"1.10.4","entries":[]}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(json).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn valid_gzip_json_passes() {
        let data = make_valid_gzip_json();
        let f = write_temp_file(&data);
        assert!(validate_kanidm_json_gzip(f.path()).is_ok());
    }

    #[test]
    fn plain_garbage_fails() {
        let f = write_temp_file(b"this is not gzip at all, just random bytes");
        let err = validate_kanidm_json_gzip(f.path()).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "GZIP_INTEGRITY_ERROR");
    }

    #[test]
    fn truncated_gzip_fails() {
        let data = make_valid_gzip_json();
        let truncated = &data[..data.len() / 2];
        let f = write_temp_file(truncated);
        let err = validate_kanidm_json_gzip(f.path()).unwrap_err();
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
        let err = validate_kanidm_json_gzip(f.path()).unwrap_err();
        assert_eq!(err.0, ExitCode::Integrity);
        assert_eq!(err.1, "INVALID_JSON_PAYLOAD");
    }
}
