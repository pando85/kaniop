use crate::crd::OAuth2ClientImageStatus;
use kaniop_k8s_util::image::ImageStatus;

impl ImageStatus for OAuth2ClientImageStatus {
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
}
