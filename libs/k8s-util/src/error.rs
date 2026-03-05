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
                matches!(
                    e.as_ref(),
                    kanidm_client::ClientError::Http(
                        kanidm_client::StatusCode::SERVICE_UNAVAILABLE
                            | kanidm_client::StatusCode::GATEWAY_TIMEOUT
                            | kanidm_client::StatusCode::TOO_MANY_REQUESTS,
                        _,
                        _
                    ) | kanidm_client::ClientError::Transport(_)
                )
            }
            Error::KubeError(_, e) => {
                matches!(
                    e.as_ref(),
                    kube::Error::Api(status) if matches!(status.code, 429 | 500 | 502 | 503 | 504)
                ) || matches!(e.as_ref(), kube::Error::Service(_))
            }
            Error::HttpError(_, e) => e.is_timeout() || e.is_connect(),
            Error::ImageDownloadError(_) => true,
            Error::FinalizerError(_, _) => true,
            Error::KubeExecError(_)
            | Error::FormattingError(_, _)
            | Error::InvalidTraceId
            | Error::MissingData(_)
            | Error::ParseError(_)
            | Error::ReceiveOutput(_)
            | Error::SerializationError(_, _)
            | Error::UrlParseError(_, _)
            | Error::Utf8Error(_, _)
            | Error::ImageError(_) => false,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
