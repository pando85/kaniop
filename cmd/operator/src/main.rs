use kaniop_k8s_util::client::new_client_with_metrics;
use kaniop_operator::controller::{
    SUBSCRIBE_BUFFER_SIZE, State as KaniopState, check_api_queryable, create_subscriber,
    set_cluster_domain, set_idm_reconcile_interval,
};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::leader_election::{LeaseLock, LeaseLockParams, acquire_lease_with_retry};
use kaniop_operator::telemetry;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{Router, get};
use clap::{Parser, crate_authors, crate_description, crate_version};
use k8s_openapi::api::core::v1::Namespace;
use kube::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use rustls::crypto::aws_lc_rs::default_provider;

async fn metrics(State(state): State<KaniopState>) -> impl IntoResponse {
    match state.metrics() {
        Ok(metrics) => (
            StatusCode::OK,
            [(
                "content-type",
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            metrics,
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to get metrics: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn healthz() -> impl IntoResponse {
    Json("healthy")
}

#[derive(Parser, Debug)]
#[command(
    name="kaniop",
    about = crate_description!(),
    version = crate_version!(),
    author = crate_authors!("\n"),
)]
struct Args {
    /// Listen address (use "::" for IPv6, "0.0.0.0" for IPv4)
    #[arg(long, default_value = "0.0.0.0", env)]
    listen_address: String,

    /// Listen on given port
    #[arg(short, long, default_value_t = 8080, env)]
    port: u16,

    /// Set logging filter directive for `tracing_subscriber::filter::EnvFilter`. Example: "info,kube=debug,kaniop=debug"
    #[arg(long, default_value = "info", env)]
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

    /// Reconciliation interval in seconds for IDM resources (groups, OAuth2 clients,
    /// person accounts, service accounts). This does not affect the Kanidm cluster
    /// reconciliation interval which remains at 24 hours.
    #[arg(long, default_value_t = 60, env)]
    idm_reconcile_interval_seconds: u64,

    /// Kubernetes cluster domain used for DNS resolution. Used to construct FQDNs
    /// for replication hostnames when no custom template is provided.
    #[arg(long, default_value = "cluster.local", env)]
    cluster_domain: String,

    /// Enable leader election for HA deployments.
    #[arg(long, default_value_t = false, env)]
    leader_election: bool,

    /// Lease name for leader election.
    #[arg(long, default_value = "kaniop-leader", env)]
    leader_election_lease_name: String,

    /// Lease namespace for leader election. Defaults to POD_NAMESPACE env var.
    #[arg(long, env)]
    leader_election_lease_namespace: Option<String>,

    /// Lease TTL in seconds for leader election.
    #[arg(long, default_value_t = 15, env)]
    leader_election_lease_ttl_seconds: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    default_provider().install_default().unwrap();

    let args: Args = Args::parse();

    set_idm_reconcile_interval(tokio::time::Duration::from_secs(
        args.idm_reconcile_interval_seconds,
    ));
    set_cluster_domain(args.cluster_domain);

    telemetry::init(
        &args.log_filter,
        args.log_format,
        args.tracing_url.as_deref(),
        args.sample_ratio,
    )
    .await?;
    debug!("Telemetry initialized");

    let exporter = kaniop_operator::prometheus_exporter::PrometheusExporter::new();
    debug!("Prometheus exporter created");
    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter.clone()).build();
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .build();
    debug!("Metrics provider initialized");

    kaniop_operator::prometheus_exporter::set_global_exporter(exporter);
    debug!("Global Prometheus exporter set");

    opentelemetry::global::set_meter_provider(provider.clone());
    let meter = opentelemetry::global::meter("kaniop");
    debug!("Global meter 'kaniop' created");

    let config = Config::infer().await?;
    let client = new_client_with_metrics(config, &meter).await?;
    let controllers = [
        kaniop_operator::kanidm::controller::CONTROLLER_ID,
        kaniop_group::controller::CONTROLLER_ID,
        kaniop_oauth2::controller::CONTROLLER_ID,
        kaniop_person::controller::CONTROLLER_ID,
        kaniop_service_account::controller::CONTROLLER_ID,
    ];

    let namespace = check_api_queryable::<Namespace>(client.clone()).await;
    let namespace_r = create_subscriber::<Namespace>(SUBSCRIBE_BUFFER_SIZE);
    let kanidm = check_api_queryable::<Kanidm>(client.clone()).await;
    let kanidm_r = create_subscriber::<Kanidm>(SUBSCRIBE_BUFFER_SIZE);

    let controller_metrics = kaniop_operator::metrics::Metrics::new(&meter, &controllers);

    let state = KaniopState::new(
        controller_metrics,
        namespace_r.store.clone(),
        kanidm_r.store.clone(),
        Some(client.clone()),
    );

    let cancellation_token = CancellationToken::new();
    let shutdown_token = cancellation_token.clone();

    if args.leader_election {
        let lease_namespace = args
            .leader_election_lease_namespace
            .or_else(|| std::env::var("POD_NAMESPACE").ok())
            .ok_or_else(|| {
                anyhow::anyhow!("POD_NAMESPACE must be set when leader election is enabled")
            })?;

        let holder_id = std::env::var("POD_NAME").unwrap_or_else(|_| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string())
        });

        let lease_lock = LeaseLock::new(
            client.clone(),
            &lease_namespace,
            LeaseLockParams {
                holder_id: holder_id.clone(),
                lease_name: args.leader_election_lease_name.clone(),
                lease_ttl: Duration::from_secs(args.leader_election_lease_ttl_seconds),
            },
        );

        tracing::info!(
            lease_name = %args.leader_election_lease_name,
            lease_namespace = %lease_namespace,
            holder_id = %holder_id,
            "waiting to acquire leader lease"
        );

        acquire_lease_with_retry(&lease_lock, Duration::from_secs(2), None).await?;

        tracing::info!("acquired leader lease; starting controllers");

        let lease_lock_clone = lease_lock.clone();
        let renew_token = cancellation_token.clone();
        let lease_ttl = Duration::from_secs(args.leader_election_lease_ttl_seconds);
        let renew_interval = lease_ttl / 3;

        let renew_handle = tokio::spawn(async move {
            loop {
                if renew_token.is_cancelled() {
                    break;
                }
                tokio::time::sleep(renew_interval).await;
                if renew_token.is_cancelled() {
                    break;
                }
                match lease_lock_clone.try_acquire_or_renew().await {
                    Ok(result) if result.acquired_lease => {
                        debug!("successfully renewed leader lease");
                    }
                    Ok(_) => {
                        tracing::warn!("lost leader lease; triggering shutdown");
                        renew_token.cancel();
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("lease renewal failed: {e:#}");
                    }
                }
            }
        });

        let kanidm_c = kaniop_operator::kanidm::controller::run(
            state.clone(),
            client.clone(),
            namespace,
            namespace_r,
            kanidm,
            kanidm_r,
        );

        let group_c = kaniop_group::controller::run(state.clone(), client.clone());
        let oauth2_c = kaniop_oauth2::controller::run(state.clone(), client.clone());
        let person_c = kaniop_person::controller::run(state.clone(), client.clone());
        let service_account_c = kaniop_service_account::controller::run(state.clone(), client);

        let app = Router::new()
            .route("/metrics", get(metrics))
            .route("/healthz", get(healthz))
            .with_state(state.clone());

        let addr = if args.listen_address.contains(':') {
            format!("[{}]:{}", args.listen_address, args.port)
        } else {
            format!("{}:{}", args.listen_address, args.port)
        };
        tracing::info!("Starting HTTP server on {}", addr);
        let listener = TcpListener::bind(&addr).await?;
        let server = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal_with_token(shutdown_token));

        let controllers_handle = tokio::spawn(async move {
            tokio::join!(kanidm_c, group_c, oauth2_c, person_c, service_account_c);
        });

        tokio::select! {
            result = server => {
                tracing::info!("HTTP server stopped");
                cancellation_token.cancel();
                result?;
            }
            _ = cancellation_token.cancelled() => {
                tracing::info!("Shutdown signal received (lost lease or external signal)");
            }
        }

        controllers_handle.abort();
        lease_lock.step_down().await.ok();
        renew_handle.abort();
    } else {
        let kanidm_c = kaniop_operator::kanidm::controller::run(
            state.clone(),
            client.clone(),
            namespace,
            namespace_r,
            kanidm,
            kanidm_r,
        );

        let group_c = kaniop_group::controller::run(state.clone(), client.clone());
        let oauth2_c = kaniop_oauth2::controller::run(state.clone(), client.clone());
        let person_c = kaniop_person::controller::run(state.clone(), client.clone());
        let service_account_c = kaniop_service_account::controller::run(state.clone(), client);

        let app = Router::new()
            .route("/metrics", get(metrics))
            .route("/healthz", get(healthz))
            .with_state(state.clone());

        let addr = if args.listen_address.contains(':') {
            format!("[{}]:{}", args.listen_address, args.port)
        } else {
            format!("{}:{}", args.listen_address, args.port)
        };
        tracing::info!("Starting HTTP server on {}", addr);
        let listener = TcpListener::bind(&addr).await?;
        let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

        tokio::join!(
            kanidm_c,
            group_c,
            oauth2_c,
            person_c,
            service_account_c,
            server
        )
        .5?;
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

async fn shutdown_signal_with_token(token: CancellationToken) {
    let mut sigterm =
        signal(SignalKind::terminate()).expect("failed to install SIGTERM signal handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            token.cancel();
        },
        _ = sigterm.recv() => {
            token.cancel();
        },
        _ = token.cancelled() => {},
    }
}
