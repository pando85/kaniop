//! Test-only fake Kanidm HTTP server and client/metrics fixtures.
//!
//! One request per connection, canned responses chosen by a handler closure,
//! every request recorded. Enough to exercise the executors against the real
//! `KanidmClient` without a Kanidm server.

use std::sync::{Arc, Mutex};

use kanidm_client::{KanidmClient, KanidmClientBuilder};
use kaniop_operator::metrics::ControllerMetrics;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The `X-KANIDM-VERSION` the client expects (its own crate version). A
/// debug-build client exits the process on a mismatch, so keep this equal to
/// the `kanidm_client` version in the workspace.
const KANIDM_VERSION: &str = "1.13.0-dev";

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

pub(crate) struct MockKanidm {
    /// Shared so a test can also hand it to a reconcile pass.
    pub client: Arc<KanidmClient>,
    pub requests: Arc<Mutex<Vec<Request>>>,
    // Keeps the metrics pipeline alive for the client's lifetime.
    pub metrics: ControllerMetrics,
    _provider: SdkMeterProvider,
}

impl MockKanidm {
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests that change state (everything except GET).
    pub fn writes(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.method != "GET")
            .collect()
    }
}

/// Start the server. The handler returns `(status, json body)`.
pub(crate) async fn start(
    handler: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static,
) -> MockKanidm {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, content_length) = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..pos]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (pos + 4, length);
                    }
                };
                while buffer.len() < head_end + content_length {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or_default().split_whitespace();
                let request = Request {
                    method: first.next().unwrap_or_default().to_string(),
                    path: first.next().unwrap_or_default().to_string(),
                    authorization: lines
                        .find(|l| l.to_lowercase().starts_with("authorization:"))
                        .map(|l| l.split_once(':').unwrap().1.trim().to_string()),
                    body: String::from_utf8_lossy(&buffer[head_end..]).to_string(),
                };
                let (status, body) = handler(&request);
                seen.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-kanidm-version: {KANIDM_VERSION}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    let client = Arc::new(
        KanidmClientBuilder::new()
            .address(format!("http://{address}"))
            .build()
            .unwrap(),
    );
    let provider = SdkMeterProvider::builder().build();
    let metrics = ControllerMetrics::new("radius-test", &provider.meter("test"));
    MockKanidm {
        client,
        requests,
        metrics,
        _provider: provider,
    }
}
