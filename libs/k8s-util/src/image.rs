use hex;
use kanidm_proto::internal::{ImageType, ImageValue};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use std::time::Duration;
use tracing::{debug, warn};

use crate::error::{Error, Result};

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

pub enum ImageOperation {
    Fetch,
    Download,
}

impl ImageOperation {
    pub fn error_reason(&self) -> &'static str {
        match self {
            ImageOperation::Fetch => "ImageFetchError",
            ImageOperation::Download => "ImageDownloadError",
        }
    }

    pub fn action(&self) -> &'static str {
        match self {
            ImageOperation::Fetch => "ImageUpdate",
            ImageOperation::Download => "ImageUpdate",
        }
    }

    pub fn operation_name(&self) -> &'static str {
        match self {
            ImageOperation::Fetch => "fetch image headers",
            ImageOperation::Download => "download image",
        }
    }
}

pub async fn publish_image_error_event<R: kube::Resource + kube::runtime::reflector::Lookup>(
    error: Error,
    operation: ImageOperation,
    resource_name: &str,
    namespace: &str,
    kanidm_name: &str,
    recorder: &kube::runtime::events::Recorder,
    resource: &R,
) -> Error
where
    <R as kube::runtime::reflector::Lookup>::DynamicType: Default,
{
    use k8s_openapi::api::core::v1::ObjectReference;
    use kube::runtime::reflector::ObjectRef;

    let msg = format!(
        "failed to {} for {} from {}/{kanidm_name}: {error:?}",
        operation.operation_name(),
        resource_name,
        namespace,
    );
    let object_ref: ObjectRef<R> = ObjectRef::from(resource);
    let object_reference: ObjectReference = object_ref.into();
    let _ = recorder
        .publish(
            &kube::runtime::events::Event {
                type_: kube::runtime::events::EventType::Warning,
                reason: operation.error_reason().to_string(),
                note: Some(msg.clone()),
                action: operation.action().to_string(),
                secondary: None,
            },
            &object_reference,
        )
        .await
        .map_err(|e| {
            warn!(%e, "failed to publish {} event", operation.error_reason());
        });
    error
}

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
    // Use the pooled client on the normal path; preserve custom timeout
    // settings for callers that explicitly override ImageConfig.
    let custom_client;
    let client = if config.http_timeout == DEFAULT_HTTP_TIMEOUT
        && config.http_connect_timeout == DEFAULT_HTTP_CONNECT_TIMEOUT
    {
        &*HTTP_CLIENT
    } else {
        custom_client = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .connect_timeout(config.http_connect_timeout)
            .build()
            .map_err(|e| Error::HttpError("failed to build HTTP client".to_string(), e))?;
        &custom_client
    };

    with_retry("fetch_headers", url, || async {
        let response = client
            .head(url)
            .send()
            .await
            .map_err(|e| Error::HttpError(format!("HEAD request failed for {url}"), e))?;

        // Periodic revalidation compares these headers with the cached ones, so only a
        // successful response can provide them: an error page carries unrelated validators
        // (or none at all) that would look like a content change. Do not fail here either,
        // many hosts answer HEAD with 403 or 405 while still serving the image on GET.
        // Report no validators and let `download_image` decide; its own status check keeps
        // an error response from becoming image content.
        if !response.status().is_success() {
            debug!(
                "HEAD request for {url} returned {}, revalidating with a full download",
                response.status()
            );
            return Ok(ImageHeaders {
                etag: None,
                last_modified: None,
                content_length: None,
                content_type: None,
            });
        }

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

        // Same reasoning as the HEAD request above: an error page is not image data, and
        // treating it as one could store a cached status that no longer matches the real
        // image. Transient 5xx responses stay retryable through `with_retry`.
        response.error_for_status_ref().map_err(|e| {
            Error::HttpError(format!("GET request returned an error status for {url}"), e)
        })?;

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
        let content_hash = hex::encode(hasher.finalize());

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
            format!("image_{}", hex::encode(hasher.finalize()))
        })
}

pub trait ImageStatus {
    fn url(&self) -> &str;
    fn etag(&self) -> Option<&String>;
    fn last_modified(&self) -> Option<&String>;
    fn content_length(&self) -> Option<u64>;
    /// SHA-256 of the stored image bytes, when recorded. Default `None` keeps
    /// implementations that only track transport validators comparable.
    fn content_hash(&self) -> Option<&str> {
        None
    }
}

pub struct UpdatedImage {
    pub image_status: ImageStatusFields,
}

#[derive(Clone)]
pub struct ImageStatusFields {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub content_hash: String,
}

impl ImageStatus for ImageStatusFields {
    fn url(&self) -> &str {
        &self.url
    }

    fn etag(&self) -> Option<&String> {
        self.etag.as_ref()
    }

    fn last_modified(&self) -> Option<&String> {
        self.last_modified.as_ref()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn content_hash(&self) -> Option<&str> {
        Some(self.content_hash.as_str())
    }
}

pub async fn update_image_if_needed<S: ImageStatus, F, Fut>(
    url: &str,
    cached_status: Option<&S>,
    update_fn: F,
) -> Result<Option<UpdatedImage>>
where
    F: FnOnce(ImageValue) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let current_headers = fetch_headers(url).await?;

    // HEAD is only sufficient to skip GET when a comparable freshness validator
    // is available. Content-Length alone cannot detect same-sized replacements,
    // and hosts that reject HEAD intentionally return empty headers above.
    // Fall back to GET and the content-hash guard when validation is unavailable.
    let should_download = match cached_status {
        None => true,
        Some(cached) => {
            let has_freshness_validator = (current_headers.etag.is_some()
                && cached.etag().is_some())
                || (current_headers.last_modified.is_some()
                    && cached.last_modified().is_some());
            cached.url() != url
                || !has_freshness_validator
                || headers_changed(&current_headers, cached)
        }
    };

    if !should_download {
        return Ok(None);
    }

    let downloaded = download_image(url).await?;

    // Validators can churn without the bytes changing (dynamic endpoints often report a
    // fresh Last-Modified for identical content, and proxies may drop ETag between HEAD
    // and GET). Writing an unchanged image would report a change, force a status patch
    // and requeue immediately: periodic revalidation would then repeat that write every
    // fast retry. Identical bytes must never be a change.
    if let Some(cached) = cached_status.filter(|cached| cached.url() == url) {
        if cached.content_hash() == Some(downloaded.content_hash.as_str()) {
            // No URL here: image URLs can carry credentials in a query string.
            debug!("image content unchanged despite differing validators, skipping update");
            return Ok(None);
        }
    }

    update_fn(downloaded.image_value).await?;

    Ok(Some(UpdatedImage {
        image_status: ImageStatusFields {
            url: url.to_string(),
            etag: downloaded.headers.etag,
            last_modified: downloaded.headers.last_modified,
            content_length: downloaded.headers.content_length,
            content_hash: downloaded.content_hash,
        },
    }))
}

pub fn headers_changed<S: ImageStatus>(current: &ImageHeaders, cached: &S) -> bool {
    if current.etag.is_some() && cached.etag().is_some() {
        return current.etag.as_ref() != cached.etag();
    }

    if current.last_modified.is_some() && cached.last_modified().is_some() {
        return current.last_modified.as_ref() != cached.last_modified();
    }

    if current.content_length.is_some() && cached.content_length().is_some() {
        return current.content_length != cached.content_length();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct CannedResponse {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Option<&'static [u8]>,
    }

    impl CannedResponse {
        fn headers_only(status: u16, headers: Vec<(&'static str, String)>) -> Self {
            Self {
                status,
                headers,
                body: None,
            }
        }
    }

    /// One request per connection, FIFO canned responses, recorded methods. Enough to
    /// exercise the HEAD/GET paths against a real HTTP client without an image host.
    async fn spawn_mock(responses: Vec<CannedResponse>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let queue = Arc::new(Mutex::<VecDeque<CannedResponse>>::new(responses.into()));
        let methods: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = methods.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                let request_read = loop {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break false,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                                break true;
                            }
                        }
                    }
                };
                if !request_read {
                    return;
                }
                let Some(method) = String::from_utf8_lossy(&buffer)
                    .split_whitespace()
                    .next()
                    .map(str::to_string)
                else {
                    continue;
                };
                seen.lock().unwrap().push(method.clone());
                let Some(response) = queue.lock().unwrap().pop_front() else {
                    return;
                };
                // HEAD must not carry a body even when it announces a content length.
                let mut head = format!("HTTP/1.1 {} Status\r\n", response.status);
                for (name, value) in &response.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("connection: close\r\n\r\n");
                if stream.write_all(head.as_bytes()).await.is_err() {
                    continue;
                }
                if method == "GET"
                    && let Some(body) = response.body
                {
                    let _ = stream.write_all(body).await;
                }
                let _ = stream.flush().await;
            }
        });
        (format!("http://{addr}/logo.svg"), methods)
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    #[tokio::test]
    async fn test_fetch_headers_ignores_unsuccessful_response() {
        // Error pages announce unrelated validators that would look like a content change on
        // every revalidation pass, so they must never be reported as the current state of
        // the image. Failing here would be worse than ignoring them: plenty of hosts answer
        // HEAD with 403 or 405 while serving the image on GET.
        let (url, methods) = spawn_mock(vec![CannedResponse::headers_only(
            405,
            vec![
                ("etag", "\"error-page\"".to_string()),
                ("content-type", "text/html".to_string()),
                ("content-length", "9".to_string()),
            ],
        )])
        .await;

        let headers = fetch_headers(&url).await.expect("HEAD 405 is recoverable");
        assert_eq!(
            headers.etag, None,
            "error page validators are not validators"
        );
        assert_eq!(headers.last_modified, None);
        assert_eq!(headers.content_length, None);
        // 405 is permanent: no retry traffic against a host that will never answer.
        assert_eq!(methods.lock().unwrap().clone(), ["HEAD"]);
    }

    #[tokio::test]
    async fn test_update_image_if_needed_downloads_when_head_is_rejected() {
        // The companion of the test above: a host that rejects HEAD must still be able to
        // provision an image, because the GET response is what carries the bytes.
        const IMAGE: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        let (url, methods) = spawn_mock(vec![
            CannedResponse::headers_only(405, vec![("content-type", "text/html".to_string())]),
            CannedResponse {
                status: 200,
                headers: vec![
                    ("etag", "\"get-1\"".to_string()),
                    ("content-type", "image/svg+xml".to_string()),
                    ("content-length", IMAGE.len().to_string()),
                ],
                body: Some(IMAGE),
            },
        ])
        .await;

        let written = Arc::new(AtomicBool::new(false));
        let written_in_closure = written.clone();
        let updated =
            update_image_if_needed(&url, None::<&ImageStatusFields>, move |_image| async move {
                written_in_closure.store(true, Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("a host that rejects HEAD must not block the image");

        assert!(written.load(Ordering::Relaxed));
        assert_eq!(methods.lock().unwrap().clone(), ["HEAD", "GET"]);
        match updated {
            Some(updated) => assert_eq!(updated.image_status.content_hash, sha256(IMAGE)),
            None => panic!("an image that was never stored must be an update"),
        }
    }

    #[tokio::test]
    async fn test_download_image_rejects_error_page() {
        let (url, _) = spawn_mock(vec![CannedResponse {
            status: 404,
            headers: vec![
                ("content-type", "text/html".to_string()),
                ("content-length", "9".to_string()),
            ],
            body: Some(b"not found"),
        }])
        .await;

        let error = match download_image(&url).await {
            Ok(_) => panic!("an error page is not image data"),
            Err(error) => error,
        };
        // Fails on the HTTP status rather than as a late content-type rejection: the
        // error body never reaches the image decoder or Kanidm.
        assert!(matches!(error, Error::HttpError(_, _)), "{error:?}");
    }

    #[tokio::test]
    async fn test_download_image_retries_transient_server_error() {
        // The status check has to keep the status inside the reqwest error, otherwise
        // `with_retry` sees a permanent failure and an image host that is briefly
        // unavailable stops being revalidated until the next operator restart.
        let (url, methods) = spawn_mock(vec![
            CannedResponse::headers_only(503, vec![]),
            CannedResponse::headers_only(503, vec![]),
            CannedResponse::headers_only(503, vec![]),
        ])
        .await;

        let error = match download_image(&url).await {
            Ok(_) => panic!("a server error is not image data"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::HttpError(_, _)), "{error:?}");
        assert_eq!(
            methods.lock().unwrap().len(),
            MAX_RETRIES as usize,
            "a 5xx response must stay retryable"
        );
    }

    #[tokio::test]
    async fn test_update_image_if_needed_skips_unchanged_content() {
        const IMAGE: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        // Hosts without a stable ETag (dynamic endpoints, proxies) can announce a new
        // validator while serving identical bytes. Writing that to Kanidm reports a change,
        // which forces a status patch and an immediate requeue: the loop would repeat on
        // every fast retry instead of once per reconciliation interval.
        let (url, methods) = spawn_mock(vec![
            CannedResponse::headers_only(
                200,
                vec![
                    ("etag", "\"head-1\"".to_string()),
                    ("content-length", IMAGE.len().to_string()),
                ],
            ),
            CannedResponse {
                status: 200,
                headers: vec![
                    ("etag", "\"get-1\"".to_string()),
                    ("content-type", "image/svg+xml".to_string()),
                    ("content-length", IMAGE.len().to_string()),
                ],
                body: Some(IMAGE),
            },
        ])
        .await;

        let cached = ImageStatusFields {
            url: url.clone(),
            etag: Some("\"stale\"".to_string()),
            last_modified: None,
            content_length: Some(IMAGE.len() as u64),
            content_hash: sha256(IMAGE),
        };

        let written = Arc::new(AtomicBool::new(false));
        let written_in_closure = written.clone();
        let updated = update_image_if_needed(&url, Some(&cached), move |_image| async move {
            written_in_closure.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await
        .unwrap();

        assert!(updated.is_none(), "identical bytes are not an update");
        assert!(
            !written.load(Ordering::Relaxed),
            "an unchanged image must not be written to Kanidm"
        );
        assert_eq!(methods.lock().unwrap().clone(), ["HEAD", "GET"]);
    }

    #[tokio::test]
    async fn test_update_image_if_needed_revalidates_cached_image_after_head_405() {
        // The initial image has already been applied. A host may reject HEAD
        // while still serving an updated image on GET.
        const OLD: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='16'/>";
        const NEW: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='32'/>";
        assert_eq!(OLD.len(), NEW.len());

        let (url, methods) = spawn_mock(vec![
            CannedResponse::headers_only(405, vec![("content-type", "text/html".to_string())]),
            CannedResponse {
                status: 200,
                headers: vec![
                    ("content-type", "image/svg+xml".to_string()),
                    ("content-length", NEW.len().to_string()),
                ],
                body: Some(NEW),
            },
        ])
        .await;

        let cached = ImageStatusFields {
            url: url.clone(),
            etag: Some("\"old-validator\"".to_string()),
            last_modified: None,
            content_length: Some(OLD.len() as u64),
            content_hash: sha256(OLD),
        };
        let written = Arc::new(AtomicBool::new(false));
        let written_in_closure = written.clone();
        let updated = update_image_if_needed(&url, Some(&cached), move |_image| async move {
            written_in_closure.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("a rejected HEAD must fall back to GET");

        assert!(written.load(Ordering::Relaxed));
        assert_eq!(methods.lock().unwrap().clone(), ["HEAD", "GET"]);
        assert_eq!(
            updated.expect("changed image must be applied").image_status.content_hash,
            sha256(NEW)
        );
    }

    #[tokio::test]
    async fn test_update_image_if_needed_revalidates_when_only_content_length_is_available() {
        // Same-sized content can change without changing Content-Length.
        const OLD: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='16'/>";
        const NEW: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='32'/>";
        assert_eq!(OLD.len(), NEW.len());

        let (url, methods) = spawn_mock(vec![
            CannedResponse::headers_only(
                200,
                vec![("content-length", OLD.len().to_string())],
            ),
            CannedResponse {
                status: 200,
                headers: vec![
                    ("content-type", "image/svg+xml".to_string()),
                    ("content-length", NEW.len().to_string()),
                ],
                body: Some(NEW),
            },
        ])
        .await;

        let cached = ImageStatusFields {
            url: url.clone(),
            etag: None,
            last_modified: None,
            content_length: Some(OLD.len() as u64),
            content_hash: sha256(OLD),
        };
        let written = Arc::new(AtomicBool::new(false));
        let written_in_closure = written.clone();
        let updated = update_image_if_needed(&url, Some(&cached), move |_image| async move {
            written_in_closure.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("a HEAD with no freshness validators must fall back to GET");

        assert!(written.load(Ordering::Relaxed));
        assert_eq!(methods.lock().unwrap().clone(), ["HEAD", "GET"]);
        assert_eq!(
            updated.expect("changed image must be applied").image_status.content_hash,
            sha256(NEW)
        );
    }

    #[tokio::test]
    async fn test_update_image_if_needed_writes_changed_content() {
        const OLD: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        const NEW: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='16'/>";

        let (url, _) = spawn_mock(vec![
            CannedResponse::headers_only(
                200,
                vec![
                    ("etag", "\"head-2\"".to_string()),
                    ("content-length", NEW.len().to_string()),
                ],
            ),
            CannedResponse {
                status: 200,
                headers: vec![
                    ("etag", "\"head-2\"".to_string()),
                    ("content-type", "image/svg+xml".to_string()),
                    ("content-length", NEW.len().to_string()),
                ],
                body: Some(NEW),
            },
        ])
        .await;

        let cached = ImageStatusFields {
            url: url.clone(),
            etag: Some("\"head-1\"".to_string()),
            last_modified: None,
            content_length: Some(OLD.len() as u64),
            content_hash: sha256(OLD),
        };

        let written = Arc::new(AtomicBool::new(false));
        let written_in_closure = written.clone();
        let updated = update_image_if_needed(&url, Some(&cached), move |_image| async move {
            written_in_closure.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await
        .unwrap();

        assert!(written.load(Ordering::Relaxed));
        match updated {
            Some(updated) => assert_eq!(updated.image_status.content_hash, sha256(NEW)),
            None => panic!("changed content must be reported as an update"),
        }
    }

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
    fn test_image_config_default() {
        let config = ImageConfig::default();
        assert_eq!(config.max_image_size, DEFAULT_MAX_IMAGE_SIZE);
        assert_eq!(config.http_timeout, DEFAULT_HTTP_TIMEOUT);
        assert_eq!(config.http_connect_timeout, DEFAULT_HTTP_CONNECT_TIMEOUT);
    }
}
