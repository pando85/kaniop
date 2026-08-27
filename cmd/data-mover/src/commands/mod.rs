pub mod check;
pub mod delete_plan;
pub mod discover;
pub mod download;
pub mod upload;

use kaniop_backup_core::operation::{OperationDocument, parse_operation_document};
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use tracing::error;

async fn load_operation(input: &str) -> Result<OperationDocument, i32> {
    let content = if input.trim_start().starts_with('{') {
        input.to_string()
    } else {
        tokio::fs::read_to_string(input).await.map_err(|e| {
            error!(path = %input, error = %e, "failed to read operation document");
            ExitCode::InvalidInput as i32
        })?
    };

    parse_operation_document(&content).map_err(|e| {
        error!(input = %input, error = %e, "invalid operation document");
        ExitCode::InvalidInput as i32
    })
}

async fn write_result(path: &str, result: &ResultDocument) -> Result<(), i32> {
    let json = result.to_json().map_err(|e| {
        error!(error = %e, "failed to serialize result document");
        ExitCode::Retryable as i32
    })?;

    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            error!(path = %path, error = %e, "failed to create result directory");
            ExitCode::Retryable as i32
        })?;
    }

    tokio::fs::write(path, json).await.map_err(|e| {
        error!(path = %path, error = %e, "failed to write result document");
        ExitCode::Retryable as i32
    })
}
