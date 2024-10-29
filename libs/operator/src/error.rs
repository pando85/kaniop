use thiserror::Error;

// TODO: check errors usage, naming, and messages
#[derive(Error, Debug)]
pub enum Error {
    #[error("kube error: {0}")]
    KubeError(#[source] kube::Error),

    #[error("formatting error: {0}")]
    FormattingError(#[source] std::fmt::Error),

    #[error("finalizer error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("missing object: {0}")]
    MissingObject(&'static str),

    #[error("receive output error: {0}")]
    ReceiveOutput(&'static str),

    #[error("serializing password failed with error: {0}")]
    PasswordSerializationError(#[source] serde_json::Error),

    #[error("invalid trace ID")]
    InvalidTraceId,
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
