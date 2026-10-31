//! Typed errors of the effectful RADIUS executors.
//!
//! Display text names objects and operations only: never a token, Secret
//! value or NAS credential. Transient API failures keep their source through
//! [`ExecError::Api`] so the controller backoff can classify them; everything
//! else is an expected block the reconciler reports on a condition.

use kaniop_k8s_util::error::Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecError {
    /// The Kanidm account for this CR UID exists but lacks our ownership
    /// marker. It is never adopted, updated or deleted.
    #[error("Kanidm account {account} exists but is not owned by this resource")]
    AccountNotOwned { account: String },
    /// A Kubernetes object with a name we need is controlled by something else.
    #[error("{kind} {namespace}/{name} exists but is not controlled by this resource")]
    ForeignObject {
        kind: String,
        namespace: String,
        name: String,
    },
    /// A create-only config Secret exists with different content or shape.
    #[error("config Secret {namespace}/{name} exists with different content")]
    SecretConflict { namespace: String, name: String },
    /// A delete precondition (UID or resourceVersion) failed: the object
    /// changed after it was read. Re-observe and retry.
    #[error("{kind} {namespace}/{name} changed while it was being deleted")]
    ConcurrentMutation {
        kind: String,
        namespace: String,
        name: String,
    },
    /// Kanidm did not accept a freshly minted token (`whoami` failed).
    #[error("Kanidm rejected the newly minted token for account {account}")]
    CandidateRejected { account: String },
    /// The token list after a mint does not contain exactly one new token
    /// with the label we issued.
    #[error("cannot identify the newly minted token on account {account}")]
    MintedTokenNotFound { account: String },
    /// The object is missing metadata the executor needs (name, namespace,
    /// UID or resourceVersion).
    #[error("{kind} is missing {field}")]
    MissingMetadata {
        kind: &'static str,
        field: &'static str,
    },
    /// A Kubernetes or Kanidm API call failed; the source is kept.
    #[error(transparent)]
    Api(#[from] Error),
}

impl ExecError {
    /// Whether retrying the same operation can succeed without a change in
    /// the world. Expected blocks return false.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api(error) => error.is_retryable(),
            Self::ConcurrentMutation { .. } | Self::CandidateRejected { .. } => true,
            Self::AccountNotOwned { .. }
            | Self::ForeignObject { .. }
            | Self::SecretConflict { .. }
            | Self::MintedTokenNotFound { .. }
            | Self::MissingMetadata { .. } => false,
        }
    }
}

pub type ExecResult<T> = Result<T, ExecError>;

/// Wrap a Kanidm SDK error with the operation and entity, keeping the source.
pub(crate) fn kanidm_error(
    operation: &str,
    entity: &str,
    error: kanidm_client::ClientError,
) -> ExecError {
    ExecError::Api(Error::KanidmClientError(
        format!("failed to {operation} {entity}"),
        Box::new(error),
    ))
}

/// Wrap a Kubernetes client error with the operation and object, keeping the
/// source.
pub(crate) fn kube_error(
    operation: &str,
    kind: &str,
    namespace: &str,
    name: &str,
    error: kube::Error,
) -> ExecError {
    ExecError::Api(Error::kube_error(operation, kind, namespace, name, error))
}
