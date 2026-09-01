use http::{HeaderMap, HeaderValue};
use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use s3::serde_types::ListBucketResult;
use tracing::debug;

pub const DEFAULT_PART_SIZE: usize = 8 * 1024 * 1024;

pub const SSE_HEADER_ENCRYPTION: &str = "x-amz-server-side-encryption";
pub const SSE_HEADER_KMS_KEY_ID: &str = "x-amz-server-side-encryption-aws-kms-key-id";
pub const SSE_VALUE_AES256: &str = "AES256";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseHeaders {
    pub encryption_mode: String,
    pub key_id: Option<String>,
}

impl SseHeaders {
    pub fn from_operation_fields(mode: Option<&str>, key_id: Option<&str>) -> Option<Self> {
        let mode = mode?;
        match mode {
            "providerManaged" | "providerKms" => Some(Self {
                encryption_mode: mode.to_string(),
                key_id: key_id.map(String::from),
            }),
            _ => None,
        }
    }

    pub fn apply_to_headers(&self, headers: &mut HeaderMap) {
        let sse_value = match self.encryption_mode.as_str() {
            "providerKms" => "aws:kms",
            _ => SSE_VALUE_AES256,
        };
        headers.insert(
            SSE_HEADER_ENCRYPTION,
            HeaderValue::from_str(sse_value).expect("SSE header value is valid ASCII"),
        );
        if let Some(key_id) = &self.key_id {
            if let Ok(val) = HeaderValue::from_str(key_id) {
                headers.insert(SSE_HEADER_KMS_KEY_ID, val);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("S3 operation failed: {0}")]
    Operation(String),
    #[error("credentials not configured: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY")]
    MissingCredentials,
    #[error("endpoint must use HTTPS: {0}")]
    InsecureEndpoint(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("conditional put failed: object already exists")]
    ObjectAlreadyExists,
}

pub struct S3Config {
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    pub force_path_style: bool,
    pub ca_bundle_path: Option<String>,
    pub insecure: bool,
}

pub async fn create_bucket(config: &S3Config) -> Result<Box<Bucket>, S3Error> {
    validate_endpoint(&config.endpoint, config.insecure)?;

    let region = Region::Custom {
        region: config.region.clone(),
        endpoint: config.endpoint.clone(),
    };

    let credentials = load_credentials()?;

    let mut bucket = Bucket::new(&config.bucket, region, credentials)
        .map_err(|e| S3Error::Operation(format!("failed to create bucket handle: {e}")))?;

    if config.force_path_style {
        bucket.set_path_style();
    }

    if config.ca_bundle_path.is_some() {
        debug!(
            ca_bundle = ?config.ca_bundle_path,
            "CA bundle configured but rust-s3 does not support custom CA bundles; \
             set SSL_CERT_FILE environment variable to use custom CAs"
        );
    }

    debug!(
        bucket = %config.bucket,
        endpoint = %config.endpoint,
        region = %config.region,
        path_style = config.force_path_style,
        ca_bundle = config.ca_bundle_path.is_some(),
        "S3 client configured"
    );

    Ok(bucket)
}

pub async fn put_object_with_sse(
    bucket: &Bucket,
    key: &str,
    data: &[u8],
    sse: Option<&SseHeaders>,
) -> Result<(), S3Error> {
    let mut headers = HeaderMap::new();
    if let Some(sse) = sse {
        sse.apply_to_headers(&mut headers);
    }
    if headers.is_empty() {
        bucket
            .put_object(key, data)
            .await
            .map_err(|e| S3Error::Operation(format!("put object failed: {e}")))?;
    } else {
        bucket
            .put_object_with_headers(key, data, Some(headers))
            .await
            .map_err(|e| S3Error::Operation(format!("put object with SSE failed: {e}")))?;
    }
    Ok(())
}

pub async fn initiate_multipart_upload_with_sse(
    bucket: &Bucket,
    key: &str,
    content_type: &str,
    sse: Option<&SseHeaders>,
) -> Result<s3::serde_types::InitiateMultipartUploadResponse, S3Error> {
    match sse {
        None => bucket
            .initiate_multipart_upload(key, content_type)
            .await
            .map_err(|e| S3Error::Operation(format!("initiate multipart upload failed: {e}"))),
        Some(sse) => {
            let mut headers = HeaderMap::new();
            sse.apply_to_headers(&mut headers);
            let bucket_with_headers = bucket
                .with_extra_headers(headers)
                .map_err(|e| S3Error::Operation(format!("failed to set extra headers: {e}")))?;
            bucket_with_headers
                .initiate_multipart_upload(key, content_type)
                .await
                .map_err(|e| {
                    S3Error::Operation(format!("initiate multipart upload with SSE failed: {e}"))
                })
        }
    }
}

pub async fn list_objects_page(
    bucket: &Bucket,
    prefix: &str,
    continuation_token: Option<String>,
    max_keys: usize,
) -> Result<(ListBucketResult, u16), S3Error> {
    bucket
        .list_page(
            prefix.to_string(),
            None,
            continuation_token,
            None,
            Some(max_keys),
        )
        .await
        .map_err(|e| S3Error::Operation(format!("list objects failed: {e}")))
}

fn validate_endpoint(endpoint: &str, insecure: bool) -> Result<(), S3Error> {
    if endpoint.is_empty() {
        return Err(S3Error::Operation("endpoint is empty".to_string()));
    }
    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        return Err(S3Error::InsecureEndpoint(format!(
            "endpoint must start with https:// or http://: {endpoint}"
        )));
    }
    if endpoint.starts_with("http://") && !insecure && !is_loopback_endpoint(endpoint) {
        return Err(S3Error::InsecureEndpoint(format!(
            "non-loopback endpoints must use HTTPS: {endpoint}"
        )));
    }
    Ok(())
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    let stripped = endpoint.strip_prefix("http://").unwrap_or(endpoint);
    let host = if let Some(bracket_start) = stripped.find('[') {
        if let Some(bracket_end) = stripped.find(']') {
            &stripped[bracket_start + 1..bracket_end]
        } else {
            stripped
        }
    } else {
        stripped.split(':').next().unwrap_or(stripped)
    };
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

fn load_credentials() -> Result<Credentials, S3Error> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID").ok();
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY").ok();
    let session_token = std::env::var("AWS_SESSION_TOKEN").ok();

    match (access_key, secret_key) {
        (Some(ak), Some(sk)) => {
            let creds =
                Credentials::new(Some(&ak), Some(&sk), session_token.as_deref(), None, None)
                    .map_err(|e| {
                        S3Error::Operation(format!("failed to create credentials: {e}"))
                    })?;
            Ok(creds)
        }
        _ => {
            debug!("no static credentials found; attempting default credential chain");
            Credentials::new(None, None, None, None, None).map_err(|_| S3Error::MissingCredentials)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_endpoint_is_accepted() {
        assert!(validate_endpoint("https://s3.example.com", false).is_ok());
    }

    #[test]
    fn http_loopback_is_accepted() {
        assert!(validate_endpoint("http://localhost:9000", false).is_ok());
        assert!(validate_endpoint("http://127.0.0.1:9000", false).is_ok());
        assert!(validate_endpoint("http://[::1]:9000", false).is_ok());
    }

    #[test]
    fn http_loopback_subdomain_is_rejected() {
        assert!(matches!(
            validate_endpoint("http://localhost.evil.com:9000", false),
            Err(S3Error::InsecureEndpoint(_))
        ));
        assert!(matches!(
            validate_endpoint("http://127.0.0.1.evil.com:9000", false),
            Err(S3Error::InsecureEndpoint(_))
        ));
    }

    #[test]
    fn http_non_loopback_is_rejected() {
        assert!(matches!(
            validate_endpoint("http://minio.internal:9000", false),
            Err(S3Error::InsecureEndpoint(_))
        ));
    }

    #[test]
    fn http_non_loopback_accepted_when_insecure() {
        assert!(validate_endpoint("http://minio.internal:9000", true).is_ok());
    }

    #[test]
    fn empty_endpoint_is_rejected() {
        assert!(validate_endpoint("", false).is_err());
    }

    #[test]
    fn no_scheme_is_rejected() {
        assert!(validate_endpoint("s3.example.com", false).is_err());
    }

    #[test]
    fn default_constants_are_sane() {
        assert_eq!(DEFAULT_PART_SIZE, 8 * 1024 * 1024);
    }

    #[test]
    fn sse_headers_from_operation_fields_provider_managed() {
        let sse = SseHeaders::from_operation_fields(Some("providerManaged"), None).unwrap();
        assert_eq!(sse.encryption_mode, "providerManaged");
        assert!(sse.key_id.is_none());
    }

    #[test]
    fn sse_headers_from_operation_fields_provider_kms() {
        let sse =
            SseHeaders::from_operation_fields(Some("providerKms"), Some("alias/my-key")).unwrap();
        assert_eq!(sse.encryption_mode, "providerKms");
        assert_eq!(sse.key_id.as_deref(), Some("alias/my-key"));
    }

    #[test]
    fn sse_headers_from_operation_fields_client_side_returns_none() {
        assert!(SseHeaders::from_operation_fields(Some("clientSide"), None).is_none());
    }

    #[test]
    fn sse_headers_from_operation_fields_none_mode_returns_none() {
        assert!(SseHeaders::from_operation_fields(None, None).is_none());
    }

    #[test]
    fn sse_headers_apply_to_headers_provider_managed() {
        let sse = SseHeaders {
            encryption_mode: "providerManaged".to_string(),
            key_id: None,
        };
        let mut headers = HeaderMap::new();
        sse.apply_to_headers(&mut headers);
        assert_eq!(
            headers
                .get(SSE_HEADER_ENCRYPTION)
                .unwrap()
                .to_str()
                .unwrap(),
            SSE_VALUE_AES256
        );
        assert!(headers.get(SSE_HEADER_KMS_KEY_ID).is_none());
    }

    #[test]
    fn sse_headers_apply_to_headers_provider_kms() {
        let sse = SseHeaders {
            encryption_mode: "providerKms".to_string(),
            key_id: Some("alias/my-key".to_string()),
        };
        let mut headers = HeaderMap::new();
        sse.apply_to_headers(&mut headers);
        assert_eq!(
            headers
                .get(SSE_HEADER_ENCRYPTION)
                .unwrap()
                .to_str()
                .unwrap(),
            "aws:kms"
        );
        assert_eq!(
            headers
                .get(SSE_HEADER_KMS_KEY_ID)
                .unwrap()
                .to_str()
                .unwrap(),
            "alias/my-key"
        );
    }
}
