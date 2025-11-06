use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct AdmissionReview<T> {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub request: Option<AdmissionRequest<T>>,
    pub response: Option<AdmissionResponse>,
}

#[derive(Deserialize, Serialize)]
pub struct AdmissionRequest<T> {
    pub uid: String,
    pub operation: String,
    pub object: Option<T>,
}

#[derive(Deserialize, Serialize)]
pub struct AdmissionResponse {
    pub uid: String,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize)]
pub struct Status {
    pub message: String,
}

impl AdmissionResponse {
    pub fn allow(uid: String) -> Self {
        Self {
            uid,
            allowed: true,
            status: None,
        }
    }

    pub fn deny(uid: String, message: impl Into<String>) -> Self {
        Self {
            uid,
            allowed: false,
            status: Some(Status {
                message: message.into(),
            }),
        }
    }
}

impl<T> AdmissionReview<T> {
    pub fn response(self, response: AdmissionResponse) -> AdmissionReview<()> {
        AdmissionReview {
            api_version: "admission.k8s.io/v1".to_string(),
            kind: "AdmissionReview".to_string(),
            request: None,
            response: Some(response),
        }
    }
}
