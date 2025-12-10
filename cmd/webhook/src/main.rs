use kaniop_group::crd::KanidmGroup;
use kaniop_oauth2::crd::KanidmOAuth2Client;
use kaniop_operator::telemetry;
use kaniop_person::crd::KanidmPersonAccount;
use kaniop_service_account::crd::KanidmServiceAccount;

use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::routing::{Router, get, post};
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use clap::{Parser, crate_authors, crate_description, crate_version};
use futures::StreamExt;
use kube::runtime::WatchStreamExt;
use kube::runtime::reflector;
use kube::runtime::watcher;
use kube::{Api, Client};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use rustls::ServerConfig;
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::pki_types::CertificateDer;
use tokio::signal::unix::{SignalKind, signal};

mod admission;
mod handlers;
mod resources;
mod state;
mod validator;

use state::WebhookState;

async fn livez() -> &'static str {
    "healthy"
}

fn load_tls_config(cert_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<ServerConfig> {
    let cert_file = File::open(cert_path)?;
    let key_file = File::open(key_path)?;

    let mut cert_reader = BufReader::new(cert_file);
    let mut key_reader = BufReader::new(key_file);

    let certs: Vec<CertificateDer> =
        rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;

    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| anyhow::anyhow!("No private key found in key file"))?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(config)
}

async fn watch_tls_files(
    cert_path: PathBuf,
    key_path: PathBuf,
    rustls_config: axum_server::tls_rustls::RustlsConfig,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let cert_path_clone = cert_path.clone();
    let key_path_clone = key_path.clone();

    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        let _ = rt.block_on(tx.send(()));
                    }
                    _ => {}
                }
            }
        })
        .expect("Failed to create file watcher");

        // Watch the parent directories to catch symlink updates (common in K8s)
        if let Some(cert_dir) = cert_path_clone.parent() {
            let _ = watcher.watch(cert_dir, RecursiveMode::NonRecursive);
        }
        if let Some(key_dir) = key_path_clone.parent() {
            if key_dir != cert_path_clone.parent().unwrap_or(Path::new("")) {
                let _ = watcher.watch(key_dir, RecursiveMode::NonRecursive);
            }
        }

        // Keep watcher alive
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    // Reload TLS config when file changes are detected
    while rx.recv().await.is_some() {
        // Add a small delay to ensure all files are written
        tokio::time::sleep(Duration::from_secs(5)).await;

        match load_tls_config(&cert_path, &key_path) {
            Ok(new_config) => {
                rustls_config.reload_from_config(Arc::new(new_config));
                tracing::info!("Successfully reloaded TLS certificates");
            }
            Err(e) => {
                tracing::error!("Failed to load new TLS config: {}", e);
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "kaniop-webhook",
    about = crate_description!(),
    version = crate_version!(),
    author = crate_authors!("\n"),
)]
struct Args {
    /// Listen address (use "::" for IPv6, "0.0.0.0" for IPv4)
    #[arg(long, default_value = "0.0.0.0", env)]
    listen_address: String,

    /// Listen on given port
    #[arg(short, long, default_value_t = 8443, env)]
    port: u16,

    /// Filter for log messages
    #[arg(short, long, default_value = "info", env)]
    log_filter: String,

    /// Set log format
    #[arg(long, value_enum, default_value_t = telemetry::LogFormat::Text, env)]
    log_format: telemetry::LogFormat,

    /// URL for the OpenTelemetry tracing endpoint.
    ///
    /// This optional argument specifies the URL to which traces will be sent using
    /// OpenTelemetry. If not provided, tracing will be disabled.
    #[arg(short, long, env = "OPENTELEMETRY_ENDPOINT_URL")]
    tracing_url: Option<String>,

    /// Sampling ratio for tracing.
    ///
    /// Specifies the ratio of traces to sample. A value of `1.0` will sample all traces,
    /// while a lower value will sample fewer traces. The default is `0.1`, meaning 10%
    /// of traces are sampled.
    #[arg(short, long, default_value_t = 0.1, env)]
    sample_ratio: f64,

    /// Path to TLS certificate file
    #[arg(long, env, required = true)]
    tls_cert: PathBuf,

    /// Path to TLS private key file
    #[arg(long, env, required = true)]
    tls_key: PathBuf,
}

static READYZ_READY: AtomicBool = AtomicBool::new(true);

async fn readyz() -> impl axum::response::IntoResponse {
    if READYZ_READY.load(Ordering::Relaxed) {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    default_provider().install_default().unwrap();

    let args: Args = Args::parse();

    telemetry::init(
        &args.log_filter,
        args.log_format,
        args.tracing_url.as_deref(),
        args.sample_ratio,
    )
    .await?;

    let client = Client::try_default().await?;

    let (group_store, group_writer) = reflector::store_shared::<KanidmGroup>(256);
    let (person_store, person_writer) = reflector::store_shared::<KanidmPersonAccount>(256);
    let (oauth2_store, oauth2_writer) = reflector::store_shared::<KanidmOAuth2Client>(256);
    let (sa_store, sa_writer) = reflector::store_shared::<KanidmServiceAccount>(256);

    let group_api = Api::<KanidmGroup>::all(client.clone());
    let person_api = Api::<KanidmPersonAccount>::all(client.clone());
    let oauth2_api = Api::<KanidmOAuth2Client>::all(client.clone());
    let sa_api = Api::<KanidmServiceAccount>::all(client.clone());

    let group_watcher = watcher(group_api, watcher::Config::default())
        .default_backoff()
        .reflect_shared(group_writer)
        .for_each(|_| async {});

    let person_watcher = watcher(person_api, watcher::Config::default())
        .default_backoff()
        .reflect_shared(person_writer)
        .for_each(|_| async {});

    let oauth2_watcher = watcher(oauth2_api, watcher::Config::default())
        .default_backoff()
        .reflect_shared(oauth2_writer)
        .for_each(|_| async {});

    let sa_watcher = watcher(sa_api, watcher::Config::default())
        .default_backoff()
        .reflect_shared(sa_writer)
        .for_each(|_| async {});

    let state = WebhookState::new(group_store, person_store, oauth2_store, sa_store);

    // Create router
    let app = Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route(
            "/validate-kanidm-group",
            post(handlers::validate_kanidm_group),
        )
        .route(
            "/validate-kanidm-person",
            post(handlers::validate_kanidm_person),
        )
        .route(
            "/validate-kanidm-oauth2",
            post(handlers::validate_kanidm_oauth2),
        )
        .route(
            "/validate-kanidm-service-account",
            post(handlers::validate_kanidm_service_account),
        )
        .with_state(state);

    // Run server and watchers concurrently
    let addr = format!("{}:{}", args.listen_address, args.port);
    let socket_addr: SocketAddr = addr.parse()?;

    tracing::info!("Starting HTTPS server on {}", socket_addr);
    let tls_config = load_tls_config(&args.tls_cert, &args.tls_key)?;
    let rustls_config = RustlsConfig::from_config(Arc::new(tls_config));

    let handle: Handle<SocketAddr> = Handle::new();
    let shutdown_handle = handle.clone();

    // Spawn shutdown signal handler
    tokio::spawn(async move {
        shutdown_signal().await;
        READYZ_READY.store(false, Ordering::Relaxed);
        tracing::info!("Received shutdown signal, starting graceful shutdown");
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
    });

    // Spawn TLS certificate watcher
    let tls_watcher = watch_tls_files(
        args.tls_cert.clone(),
        args.tls_key.clone(),
        rustls_config.clone(),
    );

    let server = axum_server::bind_rustls(socket_addr, rustls_config)
        .handle(handle)
        .serve(app.into_make_service());

    tokio::select! {
        result = server => { result?; },
        _ = tls_watcher => {},
        _ = group_watcher => {},
        _ = person_watcher => {},
        _ = oauth2_watcher => {},
        _ = sa_watcher => {},
    }

    Ok(())
}

async fn shutdown_signal() {
    let mut sigterm =
        signal(SignalKind::terminate()).expect("failed to install SIGTERM signal handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
}
