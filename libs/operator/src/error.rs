use thiserror::Error;

// TODO: check errors usage, naming, and messages
#[derive(Error, Debug)]
pub enum Error {
    #[error("{0} (kube error: {1})")]
    KubeError(String, #[source] kube::Error),

    #[error("{0}: {0}")]
    FormattingError(String, #[source] std::fmt::Error),

    #[error("finalizer error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("missing object: {0}")]
    MissingObject(String),

    #[error("receive output error: {0}")]
    ReceiveOutput(String),

    #[error("{0}: {0}")]
    SerializationError(String, #[source] serde_json::Error),

    #[error("invalid trace ID")]
    InvalidTraceId,
}
pub type Result<T, E = Error> = std::result::Result<T, E>;
