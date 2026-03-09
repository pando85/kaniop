use crate::crd::OAuth2ClientImageStatus;

use kanidm_proto::internal::{ImageType, ImageValue};
use kaniop_k8s_util::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use std::time::Duration;
use tracing::warn;

pub const DEFAULT_MAX_IMAGE_SIZE: u64 = 256 * 1024;
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(100);

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(DEFAULT_HTTP_TIMEOUT)
        .connect_timeout(DEFAULT_HTTP_CONNECT_TIMEOUT)
        .pool_max_idle_per_host(5)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .expect("Failed to create HTTP client")
});

#[derive(Clone, Debug)]
pub struct ImageConfig {
    pub max_image_size: u64,
    pub http_timeout: Duration,
    pub http_connect_timeout: Duration,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            max_image_size: DEFAULT_MAX_IMAGE_SIZE,
            http_timeout: DEFAULT_HTTP_TIMEOUT,
            http_connect_timeout: DEFAULT_HTTP_CONNECT_TIMEOUT,
        }
    }
}

pub struct ImageHeaders {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub content_type: Option<String>,
}

pub struct DownloadedImage {
    pub image_value: ImageValue,
    pub headers: ImageHeaders,
    pub content_hash: String,
}

fn extract_headers(response: &reqwest::Response) -> ImageHeaders {
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let last_modified = response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let content_length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    ImageHeaders {
        etag,
        last_modified,
        content_length,
        content_type,
    }
}

fn is_transient_error(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() {
        return true;
    }

    if let Some(status) = error.status() {
        return status.is_server_error();
    }

    false
}

async fn with_retry<F, T, Fut>(operation: &str, url: &str, f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let is_transient =
                    matches!(&e, Error::HttpError(_, inner) if is_transient_error(inner));

                if !is_transient || attempt == MAX_RETRIES - 1 {
                    return Err(e);
                }

                let delay = RETRY_BASE_DELAY * 2u32.pow(attempt);
                warn!(
                    operation = operation,
                    url = url,
                    attempt = attempt + 1,
                    max_retries = MAX_RETRIES,
                    delay_ms = delay.as_millis(),
                    error = ?e,
                    "Retrying after transient error"
                );
                tokio::time::sleep(delay).await;
                last_error = Some(e);
            }
        }
    }

    Err(last_error.expect("At least one error occurred"))
}

pub async fn fetch_headers(url: &str) -> Result<ImageHeaders> {
    fetch_headers_with_config(url, &ImageConfig::default()).await
}

pub async fn fetch_headers_with_config(url: &str, config: &ImageConfig) -> Result<ImageHeaders> {
    let client = reqwest::Client::builder()
        .timeout(config.http_timeout)
        .connect_timeout(config.http_connect_timeout)
        .build()
        .map_err(|e| Error::HttpError("failed to build HTTP client".to_string(), e))?;

    with_retry("fetch_headers", url, || async {
        let response = client
            .head(url)
            .send()
            .await
            .map_err(|e| Error::HttpError(format!("HEAD request failed for {url}"), e))?;

        Ok(extract_headers(&response))
    })
    .await
}

pub async fn download_image(url: &str) -> Result<DownloadedImage> {
    download_image_with_config(url, &ImageConfig::default()).await
}

pub async fn download_image_with_config(
    url: &str,
    config: &ImageConfig,
) -> Result<DownloadedImage> {
    with_retry("download_image", url, || async {
        let response = HTTP_CLIENT
            .get(url)
            .send()
            .await
            .map_err(|e| Error::HttpError(format!("GET request failed for {url}"), e))?;

        let headers = extract_headers(&response);

        if let Some(len) = headers.content_length {
            if len > config.max_image_size {
                return Err(Error::ImageError(format!(
                    "image size {len} exceeds maximum allowed size {} bytes",
                    config.max_image_size
                )));
            }
        }

        let image_type = match &headers.content_type {
            Some(ct) => ImageType::try_from_content_type(ct)
                .map_err(|e| Error::ImageError(format!("unsupported content type: {e}")))?,
            None => return Err(Error::ImageError("missing Content-Type header".to_string())),
        };

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::HttpError(format!("failed to read response body from {url}"), e))?;

        if bytes.len() as u64 > config.max_image_size {
            return Err(Error::ImageError(format!(
                "downloaded image size {} exceeds maximum allowed size {} bytes",
                bytes.len(),
                config.max_image_size
            )));
        }

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let content_hash = format!("{:x}", hasher.finalize());

        let filename = extract_filename(url);

        let image_value = ImageValue::new(filename, image_type, bytes.to_vec());

        Ok(DownloadedImage {
            image_value,
            headers: ImageHeaders {
                etag: headers.etag,
                last_modified: headers.last_modified,
                content_length: Some(bytes.len() as u64),
                content_type: headers.content_type,
            },
            content_hash,
        })
    })
    .await
}

fn extract_filename(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back().map(|s| s.to_string()))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let mut hasher = Sha256::new();
            hasher.update(url.as_bytes());
            format!("image_{:x}", hasher.finalize())
        })
}

pub fn needs_update(spec_url: &str, status: &Option<OAuth2ClientImageStatus>) -> bool {
    match status {
        None => true,
        Some(s) => s.url != spec_url,
    }
}

pub fn headers_changed(current: &ImageHeaders, cached: &OAuth2ClientImageStatus) -> bool {
    if current.etag.is_some() && cached.etag.is_some() {
        return current.etag != cached.etag;
    }

    if current.last_modified.is_some() && cached.last_modified.is_some() {
        return current.last_modified != cached.last_modified;
    }

    if current.content_length.is_some() && cached.content_length.is_some() {
        return current.content_length != cached.content_length;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_filename_from_url() {
        assert_eq!(
            extract_filename("https://example.com/path/to/image.png"),
            "image.png"
        );
        assert_eq!(
            extract_filename("https://example.com/image.svg?query=1"),
            "image.svg"
        );
    }

    #[test]
    fn test_extract_filename_from_invalid_url() {
        let result = extract_filename("not-a-url");
        assert!(result.starts_with("image_"));
    }

    #[test]
    fn test_extract_filename_from_url_without_path() {
        let result = extract_filename("https://example.com");
        assert!(result.starts_with("image_"));
    }

    #[test]
    fn test_headers_changed_with_etag() {
        let current = ImageHeaders {
            etag: Some("etag1".to_string()),
            last_modified: None,
            content_length: Some(100),
            content_type: Some("image/png".to_string()),
        };
        let cached = OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: Some("etag2".to_string()),
            last_modified: None,
            content_length: Some(100),
            content_hash: Some("hash".to_string()),
        };
        assert!(headers_changed(&current, &cached));

        let cached_same = OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: Some("etag1".to_string()),
            last_modified: None,
            content_length: Some(100),
            content_hash: Some("hash".to_string()),
        };
        assert!(!headers_changed(&current, &cached_same));
    }

    #[test]
    fn test_headers_changed_with_last_modified() {
        let current = ImageHeaders {
            etag: None,
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".to_string()),
            content_length: Some(100),
            content_type: Some("image/png".to_string()),
        };
        let cached = OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: None,
            last_modified: Some("Tue, 02 Jan 2024 00:00:00 GMT".to_string()),
            content_length: Some(100),
            content_hash: Some("hash".to_string()),
        };
        assert!(headers_changed(&current, &cached));
    }

    #[test]
    fn test_headers_changed_with_content_length() {
        let current = ImageHeaders {
            etag: None,
            last_modified: None,
            content_length: Some(100),
            content_type: Some("image/png".to_string()),
        };
        let cached = OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: None,
            last_modified: None,
            content_length: Some(200),
            content_hash: Some("hash".to_string()),
        };
        assert!(headers_changed(&current, &cached));
    }

    #[test]
    fn test_headers_changed_no_change() {
        let current = ImageHeaders {
            etag: None,
            last_modified: None,
            content_length: Some(100),
            content_type: Some("image/png".to_string()),
        };
        let cached = OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: None,
            last_modified: None,
            content_length: Some(100),
            content_hash: Some("hash".to_string()),
        };
        assert!(!headers_changed(&current, &cached));
    }

    #[test]
    fn test_needs_update() {
        assert!(needs_update("https://example.com/image.png", &None));

        let status = Some(OAuth2ClientImageStatus {
            url: "https://example.com/old.png".to_string(),
            etag: None,
            last_modified: None,
            content_length: None,
            content_hash: None,
        });
        assert!(needs_update("https://example.com/new.png", &status));

        let status_same = Some(OAuth2ClientImageStatus {
            url: "https://example.com/image.png".to_string(),
            etag: None,
            last_modified: None,
            content_length: None,
            content_hash: None,
        });
        assert!(!needs_update("https://example.com/image.png", &status_same));
    }

    #[test]
    fn test_image_config_default() {
        let config = ImageConfig::default();
        assert_eq!(config.max_image_size, DEFAULT_MAX_IMAGE_SIZE);
        assert_eq!(config.http_timeout, DEFAULT_HTTP_TIMEOUT);
        assert_eq!(config.http_connect_timeout, DEFAULT_HTTP_CONNECT_TIMEOUT);
    }
}
