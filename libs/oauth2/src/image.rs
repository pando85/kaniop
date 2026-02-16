use crate::crd::OAuth2ClientImageStatus;

use kaniop_k8s_util::error::{Error, Result};
use kanidm_proto::internal::{ImageType, ImageValue};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_IMAGE_SIZE: u64 = 256 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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

pub async fn fetch_headers(url: &str) -> Result<ImageHeaders> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| Error::HttpError("failed to build HTTP client".to_string(), e))?;

    let response = client
        .head(url)
        .send()
        .await
        .map_err(|e| Error::HttpError(format!("HEAD request failed for {url}"), e))?;

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

    Ok(ImageHeaders {
        etag,
        last_modified,
        content_length,
        content_type,
    })
}

pub async fn download_image(url: &str) -> Result<DownloadedImage> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| Error::HttpError("failed to build HTTP client".to_string(), e))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::HttpError(format!("GET request failed for {url}"), e))?;

    let content_length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    if let Some(len) = content_length {
        if len > MAX_IMAGE_SIZE {
            return Err(Error::ImageError(format!(
                "image size {len} exceeds maximum allowed size {MAX_IMAGE_SIZE} bytes"
            )));
        }
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let image_type = match &content_type {
        Some(ct) => ImageType::try_from_content_type(ct)
            .map_err(|e| Error::ImageError(format!("unsupported content type: {e}")))?,
        None => return Err(Error::ImageError("missing Content-Type header".to_string())),
    };

    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::HttpError(format!("failed to read response body from {url}"), e))?;

    if bytes.len() as u64 > MAX_IMAGE_SIZE {
        return Err(Error::ImageError(format!(
            "downloaded image size {} exceeds maximum allowed size {MAX_IMAGE_SIZE} bytes",
            bytes.len()
        )));
    }

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let content_hash = format!("{:x}", hasher.finalize());

    let etag = None;
    let last_modified = None;

    let filename = url
        .rsplit('/')
        .next()
        .unwrap_or("image")
        .split('?')
        .next()
        .unwrap_or("image")
        .to_string();

    let image_value = ImageValue::new(filename, image_type, bytes.to_vec());

    Ok(DownloadedImage {
        image_value,
        headers: ImageHeaders {
            etag,
            last_modified,
            content_length: Some(bytes.len() as u64),
            content_type,
        },
        content_hash,
    })
}

pub fn needs_update(spec_url: &str, status: &Option<OAuth2ClientImageStatus>) -> bool {
    match status {
        None => true,
        Some(s) => s.url != spec_url,
    }
}

pub fn headers_changed(
    current: &ImageHeaders,
    cached: &OAuth2ClientImageStatus,
) -> bool {
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
