//! Test-only mock Kubernetes API server over `tower_test`.

use http::{Request, Response};
use kube::client::{Body, Client};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tower_test::mock;

const NS: &str = "network";

pub(crate) struct Recorded {
    pub method: String,
    pub uri: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// Mock API server answering `replies` in order (status, body) and
/// recording every request; extra requests get a 500.
pub(crate) fn mock_client(replies: Vec<(u16, Value)>) -> (Client, JoinHandle<Vec<Recorded>>) {
    let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
    let recorder = tokio::spawn(async move {
        let mut replies = replies.into_iter();
        let mut recorded = Vec::new();
        while let Some((request, send)) = handle.next_request().await {
            let (parts, body) = request.into_parts();
            recorded.push(Recorded {
                method: parts.method.to_string(),
                uri: parts.uri.to_string(),
                content_type: parts
                    .headers
                    .get("content-type")
                    .map(|v| v.to_str().unwrap().to_string()),
                body: body.collect_bytes().await.unwrap().to_vec(),
            });
            let (status, body) = replies.next().unwrap_or((500, json!({})));
            send.send_response(
                Response::builder()
                    .status(status)
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            );
        }
        recorded
    });
    (Client::new(service, NS), recorder)
}

pub(crate) fn api_status(code: u16, reason: &str) -> (u16, Value) {
    (
        code,
        json!({
            "kind": "Status", "apiVersion": "v1", "status": "Failure",
            "message": reason, "reason": reason, "code": code
        }),
    )
}

pub(crate) fn to_json<T: Serialize>(object: &T) -> Value {
    serde_json::to_value(object).unwrap()
}
