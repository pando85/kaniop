//! Typed errors of the RADIUS executors and of one reconcile pass.
//!
//! Display text names objects and operations only: never a token, Secret
//! value or NAS credential. Transient API failures keep their source through
//! [`ExecError::Api`] and [`Failure::Transient`] so the controller backoff can
//! classify them; everything else is an expected [`RadiusBlock`] the
//! reconciler reports on a condition with a stable reason.

use std::time::Duration;

use kaniop_k8s_util::error::Error;
use thiserror::Error;

use crate::reconcile::identity::RadiusIdentityError;
use crate::reconcile::image::RadiusImageError;
use crate::reconcile::inputs::{InputsError, RadiusPreflightError};
use crate::reconcile::status::ConditionKind;
use crate::reconcile::token::MAX_TOKENS;
use crate::reconcile::workload::RadiusWorkloadError;

/// Requeue after a block that clears without a change in the world.
const RETRY_CONCURRENT: Duration = Duration::from_secs(5);
const RETRY_CANDIDATE: Duration = Duration::from_secs(30);
const RETRY_TOKEN_BOUND: Duration = Duration::from_secs(60);

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

/// An expected reason a reconcile pass stops. Each variant maps to the
/// condition it is reported on and a stable reason string; the Display text
/// is the condition message.
#[derive(Debug, Error)]
pub enum RadiusBlock {
    #[error("referenced Kanidm {kanidm} does not exist")]
    KanidmNotFound { kanidm: String },
    #[error("namespace is not selected by radiusServerNamespaceSelector of Kanidm {kanidm}")]
    NamespaceNotWatched { kanidm: String },
    #[error("{0}")]
    Inputs(#[from] RadiusPreflightError),
    #[error("{0}")]
    Image(#[from] RadiusImageError),
    #[error("{0}")]
    Workload(#[from] RadiusWorkloadError),
    #[error("{0}")]
    Identity(#[from] RadiusIdentityError),
    #[error(
        "a new token is needed but the account already holds {MAX_TOKENS} tokens; waiting for \
         unused tokens to be revoked"
    )]
    TooManyTokens,
    #[error("the config Secret of the Deployment template has no readable token")]
    BearerUnreadable,
    /// An executor block (never [`ExecError::Api`]; see [`Failure::exec`])
    /// raised while establishing `stage`.
    #[error("{error}")]
    Exec {
        #[source]
        error: ExecError,
        stage: ConditionKind,
    },
}

impl RadiusBlock {
    /// Stable condition reason.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::KanidmNotFound { .. } => "KanidmNotFound",
            Self::NamespaceNotWatched { .. } => "NamespaceNotWatched",
            Self::Inputs(error) => error.reason(),
            Self::Image(_) => "ImageUnresolved",
            Self::Workload(_) => "InvalidWorkload",
            Self::Identity(_) => "InvalidIdentity",
            Self::TooManyTokens => "TooManyTokens",
            Self::BearerUnreadable => "BearerUnreadable",
            Self::Exec { error, .. } => match error {
                ExecError::AccountNotOwned { .. } => "OwnershipConflict",
                ExecError::ForeignObject { .. } => "ForeignObject",
                ExecError::SecretConflict { .. } => "SecretConflict",
                ExecError::ConcurrentMutation { .. } => "ConcurrentMutation",
                ExecError::CandidateRejected { .. } => "CandidateRejected",
                ExecError::MintedTokenNotFound { .. } => "MintedTokenNotFound",
                ExecError::MissingMetadata { .. } => "MissingMetadata",
                ExecError::Api(_) => "TransientError",
            },
        }
    }

    /// The condition this block is reported on. An executor block lands on
    /// the condition of the object it concerns (account, Deployment,
    /// Service, a rotation-only failure), otherwise on the stage that raised
    /// it.
    pub fn condition(&self) -> ConditionKind {
        match self {
            Self::KanidmNotFound { .. } | Self::NamespaceNotWatched { .. } | Self::Identity(_) => {
                ConditionKind::IdentityReady
            }
            Self::TooManyTokens | Self::BearerUnreadable => ConditionKind::Rotating,
            Self::Inputs(_) | Self::Image(_) | Self::Workload(_) => {
                ConditionKind::ConfigurationReady
            }
            Self::Exec { error, stage } => match error {
                ExecError::AccountNotOwned { .. } => ConditionKind::IdentityReady,
                ExecError::ForeignObject { kind, .. } if kind == "Deployment" => {
                    ConditionKind::Available
                }
                ExecError::ForeignObject { kind, .. } if kind == "Service" => {
                    ConditionKind::Exposed
                }
                ExecError::CandidateRejected { .. } | ExecError::MintedTokenNotFound { .. } => {
                    ConditionKind::Rotating
                }
                _ => *stage,
            },
        }
    }

    /// Requeue delay for blocks expected to clear on their own; `None` waits
    /// for a watch event or the periodic reconcile.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Exec {
                error: ExecError::ConcurrentMutation { .. },
                ..
            } => Some(RETRY_CONCURRENT),
            Self::Exec {
                error: ExecError::CandidateRejected { .. },
                ..
            } => Some(RETRY_CANDIDATE),
            Self::TooManyTokens => Some(RETRY_TOKEN_BOUND),
            _ => None,
        }
    }
}

/// Why a reconcile pass stopped early.
#[derive(Debug, Error)]
pub enum Failure {
    /// Expected: reported on a condition, requeued without backoff.
    #[error(transparent)]
    Block(#[from] RadiusBlock),
    /// A Kubernetes or Kanidm API call failed. The condition of the stage is
    /// written first, then the error (with its source) drives backoff.
    #[error("{error}")]
    Transient {
        condition: ConditionKind,
        error: Error,
    },
}

impl Failure {
    /// Classify an executor error raised while establishing `condition`.
    pub fn exec(condition: ConditionKind) -> impl FnOnce(ExecError) -> Failure {
        move |error| match error {
            ExecError::Api(error) => Failure::Transient { condition, error },
            error => Failure::Block(RadiusBlock::Exec {
                error,
                stage: condition,
            }),
        }
    }

    /// Classify an input preparation error.
    pub fn inputs(error: InputsError) -> Failure {
        match error {
            InputsError::Invalid(error) => Failure::Block(RadiusBlock::Inputs(error)),
            InputsError::Api(error) => Failure::Transient {
                condition: ConditionKind::ConfigurationReady,
                error,
            },
        }
    }

    /// A transient failure raised while establishing `condition`.
    pub fn transient(condition: ConditionKind) -> impl FnOnce(Error) -> Failure {
        move |error| Failure::Transient { condition, error }
    }
}
