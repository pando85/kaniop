use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}: {1:?}")]
    // Boxing this error because the size can be large
    KanidmClientError(String, Box<kanidm_client::ClientError>),

    #[error("{0}: {1:?}")]
    KubeError(String, #[source] Box<kube::Error>),

    #[error("kube exec error: {0}")]
    KubeExecError(String),

    #[error("{0}: {1}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(
        String,
        #[source] Box<kube::runtime::finalizer::Error<Error>>,
    ),

    #[error("{0}: {1}")]
    FormattingError(String, #[source] std::fmt::Error),

    #[error("invalid trace ID")]
    InvalidTraceId,

    #[error("{0}")]
    MissingData(String),

    #[error("parse error: {0}")]
    ParseError(String),

    #[error("receive output error: {0}")]
    ReceiveOutput(String),

    #[error("{0}: {1}")]
    SerializationError(String, #[source] serde_json::Error),

    #[error("{0}: {1}")]
    UrlParseError(String, #[source] url::ParseError),

    #[error("{0}: {1}")]
    Utf8Error(String, #[source] std::str::Utf8Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
