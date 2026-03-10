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

    #[error("{0}: {1}")]
    HttpError(String, #[source] reqwest::Error),

    #[error("image error: {0}")]
    ImageError(String),

    #[error("image download error: {0}")]
    ImageDownloadError(String),
}

impl Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::KanidmClientError(_, e) => {
                matches!(&**e, kanidm_client::ClientError::Http(_, _, _))
            }
            Error::KubeError(_, e) => matches!(&**e, kube::Error::Api(_)),
            Error::KubeExecError(_) => true,
            Error::FinalizerError(_, _) => true,
            Error::FormattingError(_, _) => false,
            Error::InvalidTraceId => false,
            Error::MissingData(_) => false,
            Error::ParseError(_) => false,
            Error::ReceiveOutput(_) => true,
            Error::SerializationError(_, _) => false,
            Error::UrlParseError(_, _) => false,
            Error::Utf8Error(_, _) => false,
            Error::HttpError(_, e) => e.is_timeout() || e.is_connect(),
            Error::ImageError(_) => false,
            Error::ImageDownloadError(_) => true,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
