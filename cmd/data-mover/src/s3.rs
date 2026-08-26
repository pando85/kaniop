use http::{HeaderMap, HeaderValue};
use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use s3::serde_types::ListBucketResult;
use tracing::debug;

pub const DEFAULT_PART_SIZE: usize = 8 * 1024 * 1024;

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
}

pub async fn create_bucket(config: &S3Config) -> Result<Box<Bucket>, S3Error> {
    validate_endpoint(&config.endpoint)?;

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

pub async fn put_object_conditional(
    bucket: &Bucket,
    key: &str,
    data: &[u8],
    content_type: &str,
) -> Result<(), S3Error> {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::IF_NONE_MATCH, HeaderValue::from_static("*"));

    let response = bucket
        .put_object_with_content_type_and_headers(key, data, content_type, Some(headers))
        .await
        .map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("PreconditionFailed") || err_str.contains("412") {
                S3Error::ObjectAlreadyExists
            } else {
                S3Error::Operation(format!("conditional put failed: {e}"))
            }
        })?;

    if response.status_code() == 412 {
        return Err(S3Error::ObjectAlreadyExists);
    }
    if response.status_code() >= 400 {
        return Err(S3Error::Operation(format!(
            "conditional put returned status {}",
            response.status_code()
        )));
    }

    Ok(())
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

pub async fn probe_conditional_put_capability(bucket: &Bucket, probe_key: &str) -> bool {
    let probe_data = b"conditional-probe";

    let first_put =
        put_object_conditional(bucket, probe_key, probe_data, "application/octet-stream").await;
    if first_put.is_err() {
        return false;
    }

    let second_put =
        put_object_conditional(bucket, probe_key, probe_data, "application/octet-stream").await;
    let conditional_works = matches!(second_put, Err(S3Error::ObjectAlreadyExists));

    let _ = bucket.delete_object(probe_key).await;

    conditional_works
}

fn validate_endpoint(endpoint: &str) -> Result<(), S3Error> {
    if endpoint.is_empty() {
        return Err(S3Error::Operation("endpoint is empty".to_string()));
    }
    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        return Err(S3Error::InsecureEndpoint(format!(
            "endpoint must start with https:// or http://: {endpoint}"
        )));
    }
    if endpoint.starts_with("http://") && !is_loopback_endpoint(endpoint) {
        return Err(S3Error::InsecureEndpoint(format!(
            "non-loopback endpoints must use HTTPS: {endpoint}"
        )));
    }
    Ok(())
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    let stripped = endpoint.strip_prefix("http://").unwrap_or(endpoint);
    let host = stripped
        .split(':')
        .next()
        .unwrap_or(stripped)
        .trim_start_matches('[')
        .trim_end_matches(']');
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
        assert!(validate_endpoint("https://s3.example.com").is_ok());
    }

    #[test]
    fn http_loopback_is_accepted() {
        assert!(validate_endpoint("http://localhost:9000").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:9000").is_ok());
        assert!(validate_endpoint("http://[::1]:9000").is_ok());
    }

    #[test]
    fn http_loopback_subdomain_is_rejected() {
        assert!(matches!(
            validate_endpoint("http://localhost.evil.com:9000"),
            Err(S3Error::InsecureEndpoint(_))
        ));
        assert!(matches!(
            validate_endpoint("http://127.0.0.1.evil.com:9000"),
            Err(S3Error::InsecureEndpoint(_))
        ));
    }

    #[test]
    fn http_non_loopback_is_rejected() {
        assert!(matches!(
            validate_endpoint("http://minio.internal:9000"),
            Err(S3Error::InsecureEndpoint(_))
        ));
    }

    #[test]
    fn empty_endpoint_is_rejected() {
        assert!(validate_endpoint("").is_err());
    }

    #[test]
    fn no_scheme_is_rejected() {
        assert!(validate_endpoint("s3.example.com").is_err());
    }

    #[test]
    fn default_constants_are_sane() {
        assert_eq!(DEFAULT_PART_SIZE, 8 * 1024 * 1024);
    }
}
