use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0} (kube error: {1})")]
    KubeError(String, #[source] kube::Error),

    #[error("{0}: {0}")]
    FormattingError(String, #[source] std::fmt::Error),

    #[error("receive output error: {0}")]
    ReceiveOutput(String),

    #[error("{0}: {0}")]
    SerializationError(String, #[source] serde_json::Error),

    #[error("invalid trace ID")]
    InvalidTraceId,
}
pub type Result<T, E = Error> = std::result::Result<T, E>;
