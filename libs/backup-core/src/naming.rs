//! Stable names for catalogued backup records.
//!
//! Historical backup CRs use `kb-<first eight backup-id characters>`. Keep
//! this name for new backups unless it is already taken by a different
//! repository/manifest. A second repository containing the same backup gets
//! a deterministic suffix derived from the repository name, preserving
//! compatibility with existing references.

use sha2::{Digest, Sha256};

pub fn legacy_backup_name(backup_id: &str) -> String {
    format!("kb-{}", backup_id.chars().take(8).collect::<String>())
}

/// Name for a backup whose legacy name is occupied by a different object.
/// The repository name is stable even if the backup is rediscovered by another
/// schedule. A six-hex-character suffix keeps names within Kubernetes' limit.
pub fn repository_scoped_backup_name(backup_id: &str, repository_name: &str) -> String {
    let digest = Sha256::digest(repository_name.as_bytes());
    let hex = format!("{digest:x}");
    format!("{}-{}", legacy_backup_name(backup_id), &hex[..6])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "019c7c76-f423-7a12-8f41-2bea7588a303";

    #[test]
    fn legacy_name_is_unchanged() {
        assert_eq!(legacy_backup_name(ID), "kb-019c7c76");
    }

    #[test]
    fn repository_suffix_is_stable_and_distinguishes_repositories() {
        let a = repository_scoped_backup_name(ID, "offsite-a");
        let b = repository_scoped_backup_name(ID, "offsite-b");
        assert!(a.starts_with("kb-019c7c76-"));
        assert_eq!(a.len(), "kb-019c7c76-".len() + 6);
        assert_eq!(a, repository_scoped_backup_name(ID, "offsite-a"));
        assert_ne!(a, b);
    }
}
