use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("bucket is empty")]
    EmptyBucket,
    #[error("prefix contains path traversal")]
    PathTraversal,
    #[error("key escapes repository prefix")]
    KeyEscape,
    #[error("invalid component: {0}")]
    InvalidComponent(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryPath {
    bucket: String,
    prefix: String,
}

impl RepositoryPath {
    pub fn new(bucket: &str, prefix: &str) -> Result<Self, PathError> {
        if bucket.is_empty() {
            return Err(PathError::EmptyBucket);
        }
        let normalized_prefix = normalize_prefix(prefix);
        validate_no_traversal(&normalized_prefix)?;
        Ok(Self {
            bucket: bucket.to_string(),
            prefix: normalized_prefix,
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn root(&self) -> String {
        if self.prefix.is_empty() {
            "v1/".to_string()
        } else {
            format!("{}/v1/", self.prefix)
        }
    }

    pub fn tenant_path(&self, namespace_uid: &str) -> Result<String, PathError> {
        validate_component(namespace_uid, "namespaceUid")?;
        Ok(format!("{}tenants/{}/", self.root(), namespace_uid))
    }

    pub fn cluster_path(&self, namespace_uid: &str, kanidm_uid: &str) -> Result<String, PathError> {
        validate_component(namespace_uid, "namespaceUid")?;
        validate_component(kanidm_uid, "kanidmUid")?;
        Ok(format!(
            "{}tenants/{}/clusters/{}/",
            self.root(),
            namespace_uid,
            kanidm_uid
        ))
    }

    pub fn backup_path(
        &self,
        namespace_uid: &str,
        kanidm_uid: &str,
        backup_id: &str,
    ) -> Result<String, PathError> {
        validate_component(backup_id, "backupId")?;
        Ok(format!(
            "{}backups/{}/",
            self.cluster_path(namespace_uid, kanidm_uid)?,
            backup_id
        ))
    }

    pub fn payload_key(
        &self,
        namespace_uid: &str,
        kanidm_uid: &str,
        backup_id: &str,
        filename: &str,
    ) -> Result<String, PathError> {
        validate_component(filename, "filename")?;
        Ok(format!(
            "{}payload/{}",
            self.backup_path(namespace_uid, kanidm_uid, backup_id)?,
            filename
        ))
    }

    pub fn manifest_key(
        &self,
        namespace_uid: &str,
        kanidm_uid: &str,
        backup_id: &str,
    ) -> Result<String, PathError> {
        Ok(format!(
            "{}manifest.json",
            self.backup_path(namespace_uid, kanidm_uid, backup_id)?
        ))
    }

    pub fn staging_path(
        &self,
        namespace_uid: &str,
        kanidm_uid: &str,
        backup_id: &str,
    ) -> Result<String, PathError> {
        Ok(format!(
            "{}staging/{}/",
            self.cluster_path(namespace_uid, kanidm_uid)?,
            backup_id
        ))
    }

    pub fn manifests_prefix(
        &self,
        namespace_uid: &str,
        kanidm_uid: &str,
    ) -> Result<String, PathError> {
        Ok(format!(
            "{}backups/",
            self.cluster_path(namespace_uid, kanidm_uid)?
        ))
    }

    pub fn contains_key(&self, key: &str) -> bool {
        let full_prefix = if self.prefix.is_empty() {
            "v1/"
        } else {
            &format!("{}/v1/", self.prefix)
        };
        key.starts_with(full_prefix) && !key.contains("..")
    }
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s3://{}/{}", self.bucket, self.prefix)
    }
}

fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    trimmed.to_string()
}

fn validate_no_traversal(path: &str) -> Result<(), PathError> {
    if path.contains("..") {
        return Err(PathError::PathTraversal);
    }
    Ok(())
}

fn validate_component(value: &str, name: &str) -> Result<(), PathError> {
    if value.is_empty() {
        return Err(PathError::InvalidComponent(format!("{name} is empty")));
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(PathError::InvalidComponent(format!(
            "{name} contains invalid characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_path_construction() {
        let rp = RepositoryPath::new("my-bucket", "prod").unwrap();
        assert_eq!(rp.bucket(), "my-bucket");
        assert_eq!(rp.prefix(), "prod");
        assert_eq!(rp.root(), "prod/v1/");
    }

    #[test]
    fn empty_prefix() {
        let rp = RepositoryPath::new("my-bucket", "").unwrap();
        assert_eq!(rp.root(), "v1/");
    }

    #[test]
    fn prefix_slashes_are_normalized() {
        let rp = RepositoryPath::new("b", "/leading/trailing/").unwrap();
        assert_eq!(rp.prefix(), "leading/trailing");
        assert_eq!(rp.root(), "leading/trailing/v1/");
    }

    #[test]
    fn empty_bucket_is_rejected() {
        assert!(matches!(
            RepositoryPath::new("", "p"),
            Err(PathError::EmptyBucket)
        ));
    }

    #[test]
    fn traversal_in_prefix_is_rejected() {
        assert!(matches!(
            RepositoryPath::new("b", "../etc"),
            Err(PathError::PathTraversal)
        ));
    }

    #[test]
    fn full_backup_path() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let path = rp.backup_path("ns-uid", "k-uid", "backup-1").unwrap();
        assert_eq!(
            path,
            "prod/v1/tenants/ns-uid/clusters/k-uid/backups/backup-1/"
        );
    }

    #[test]
    fn payload_key_construction() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let key = rp
            .payload_key("ns", "k", "b1", "kanidm.backup.json.gz")
            .unwrap();
        assert_eq!(
            key,
            "prod/v1/tenants/ns/clusters/k/backups/b1/payload/kanidm.backup.json.gz"
        );
    }

    #[test]
    fn manifest_key_construction() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let key = rp.manifest_key("ns", "k", "b1").unwrap();
        assert_eq!(
            key,
            "prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json"
        );
    }

    #[test]
    fn staging_path_construction() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let path = rp.staging_path("ns", "k", "b1").unwrap();
        assert_eq!(path, "prod/v1/tenants/ns/clusters/k/staging/b1/");
    }

    #[test]
    fn contains_key_validates_confinement() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        assert!(rp.contains_key("prod/v1/tenants/ns/clusters/k/backups/b1/manifest.json"));
        assert!(!rp.contains_key("prod/v2/something"));
        assert!(!rp.contains_key("other/v1/something"));
        assert!(!rp.contains_key("prod/v1/tenants/../clusters/k"));
    }

    #[test]
    fn invalid_component_is_rejected() {
        let rp = RepositoryPath::new("b", "p").unwrap();
        assert!(rp.backup_path("", "k", "b").is_err());
        assert!(rp.backup_path("ns", "k", "b/id").is_err());
        assert!(rp.backup_path("ns", "k", "b..id").is_err());
    }

    #[test]
    fn manifests_prefix_for_discovery() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let prefix = rp.manifests_prefix("ns", "k").unwrap();
        assert_eq!(prefix, "prod/v1/tenants/ns/clusters/k/backups/");
    }

    #[test]
    fn display_format() {
        let rp = RepositoryPath::new("my-bucket", "prod").unwrap();
        assert_eq!(format!("{rp}"), "s3://my-bucket/prod");
    }

    #[test]
    fn contains_key_with_empty_prefix() {
        let rp = RepositoryPath::new("b", "").unwrap();
        assert!(rp.contains_key("v1/tenants/ns/clusters/k/backups/b1/manifest.json"));
        assert!(!rp.contains_key("v2/something"));
        assert!(!rp.contains_key("other/v1/something"));
    }

    #[test]
    fn contains_key_rejects_traversal() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        assert!(!rp.contains_key("prod/v1/tenants/ns/../../etc/passwd"));
    }

    #[test]
    fn cluster_path_construction() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let path = rp.cluster_path("ns-uid", "k-uid").unwrap();
        assert_eq!(path, "prod/v1/tenants/ns-uid/clusters/k-uid/");
    }

    #[test]
    fn tenant_path_construction() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let path = rp.tenant_path("ns-uid").unwrap();
        assert_eq!(path, "prod/v1/tenants/ns-uid/");
    }

    #[test]
    fn component_with_slash_is_rejected() {
        let rp = RepositoryPath::new("b", "p").unwrap();
        assert!(rp.tenant_path("ns/uid").is_err());
        assert!(rp.cluster_path("ns", "k/uid").is_err());
    }

    #[test]
    fn component_with_backslash_is_rejected() {
        let rp = RepositoryPath::new("b", "p").unwrap();
        assert!(rp.tenant_path(r"ns\uid").is_err());
    }

    #[test]
    fn manifests_prefix_ends_with_backups() {
        let rp = RepositoryPath::new("b", "prod").unwrap();
        let prefix = rp.manifests_prefix("ns", "k").unwrap();
        assert!(prefix.ends_with("backups/"));
    }
}
