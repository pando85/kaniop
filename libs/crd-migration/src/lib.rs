pub mod backup;
pub mod checksum;
pub mod crd;
pub mod migration;
pub mod sanitize;
pub mod state;
pub mod verify;

pub const LEGACY_PLURAL: &str = "kanidmpersonsaccounts";
pub const CORRECTED_PLURAL: &str = "kanidmpersonaccounts";
pub const API_GROUP: &str = "kaniop.rs";
pub const API_VERSION: &str = "v1beta1";
pub const KIND: &str = "KanidmPersonAccount";
pub const LEGACY_FINALIZER: &str = "kanidmpersonsaccounts.kaniop.rs/finalizer";
pub const LEGACY_CRD_NAME: &str = "kanidmpersonsaccounts.kaniop.rs";
pub const CORRECTED_CRD_NAME: &str = "kanidmpersonaccounts.kaniop.rs";
pub const MIGRATION_VERSION: &str = "person-plural-v1";
pub const MIGRATION_LABEL: &str = "kaniop.rs/migration";
pub const MIGRATION_SOURCE_ANNOTATION: &str = "kaniop.rs/migration-source";
pub const MIGRATION_SOURCE_NS_HASH_LABEL: &str = "kaniop.rs/migration-source-namespace-hash";
pub const DEFAULT_MARKER_NAME: &str = "kaniop-person-crd-migration";
pub const BACKUP_PREFIX: &str = "kaniop-person-backup-";

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("kube API error: {0}")]
    Kube(String, #[source] Box<kube::Error>),

    #[error("serialization error: {0}")]
    Serialization(String, #[source] serde_json::Error),

    #[error("YAML parse error: {0}")]
    YamlParse(String, #[source] serde_yaml::Error),

    #[error("state error: {0}")]
    State(String),

    #[error("backup error: {0}")]
    Backup(String),

    #[error("sanitization error: {0}")]
    Sanitize(String),

    #[error("verification error: {0}")]
    Verify(String),

    #[error("CRD error: {0}")]
    Crd(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("unknown finalizer on {ns}/{name}: {finalizer}")]
    UnknownFinalizer {
        ns: String,
        name: String,
        finalizer: String,
    },

    #[error("checksum mismatch for {ns}/{name}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        ns: String,
        name: String,
        expected: String,
        actual: String,
    },

    #[error("backup count {backup_count} does not match source count {source_count}")]
    BackupCountMismatch {
        backup_count: usize,
        source_count: usize,
    },

    #[error("duplicate source entry: {0}")]
    DuplicateSource(String),

    #[error("injected failure after phase {0}")]
    InjectedFailure(String),
}

impl MigrationError {
    pub fn kube(operation: &str, resource: &str, ns: &str, name: &str, error: kube::Error) -> Self {
        MigrationError::Kube(
            format!("failed to {operation} {resource} {ns}/{name}"),
            Box::new(error),
        )
    }
}

pub type Result<T, E = MigrationError> = std::result::Result<T, E>;
