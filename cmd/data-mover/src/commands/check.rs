use std::path::Path;

use kaniop_backup_core::operation::OperationSpec;
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use tracing::{error, info};

use crate::checksum::compute_sha256;

use super::{load_operation, write_result};

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
