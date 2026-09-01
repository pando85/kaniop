use std::path::Path;

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

#[derive(Debug, thiserror::Error)]
pub enum ChecksumError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Mismatch { expected: String, actual: String },
    #[error("size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
}

pub struct ChecksumResult {
    pub sha256: String,
    pub size_bytes: u64,
}

pub async fn compute_sha256(path: &Path) -> Result<ChecksumResult, ChecksumError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total_bytes: u64 = 0;

    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        total_bytes += bytes_read as u64;
    }

    let hash = hasher.finalize();
    let sha256 = hex::encode(hash);

    Ok(ChecksumResult {
        sha256,
        size_bytes: total_bytes,
    })
}

pub fn verify_checksum(
    actual_sha256: &str,
    expected_sha256: &str,
    actual_size: u64,
    expected_size: u64,
) -> Result<(), ChecksumError> {
    if actual_size != expected_size {
        return Err(ChecksumError::SizeMismatch {
            expected: expected_size,
            actual: actual_size,
        });
    }
    if actual_sha256 != expected_sha256 {
        return Err(ChecksumError::Mismatch {
            expected: expected_sha256.to_string(),
            actual: actual_sha256.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn compute_sha256_of_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello world").unwrap();

        let result = compute_sha256(&path).await.unwrap();
        assert_eq!(result.size_bytes, 11);
        assert_eq!(
            result.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[tokio::test]
    async fn compute_sha256_of_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::File::create(&path).unwrap();

        let result = compute_sha256(&path).await.unwrap();
        assert_eq!(result.size_bytes, 0);
        assert_eq!(
            result.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_checksum_matching() {
        assert!(verify_checksum("abc", "abc", 100, 100).is_ok());
    }

    #[test]
    fn verify_checksum_mismatch() {
        assert!(matches!(
            verify_checksum("abc", "def", 100, 100),
            Err(ChecksumError::Mismatch { .. })
        ));
    }

    #[test]
    fn verify_size_mismatch() {
        assert!(matches!(
            verify_checksum("abc", "abc", 100, 200),
            Err(ChecksumError::SizeMismatch { .. })
        ));
    }
}
