use k8s_openapi::api::core::v1::Namespace;
use kaniop_k8s_util::client::new_client_with_metrics;
use kaniop_operator::controller::{
    SUBSCRIBE_BUFFER_SIZE, State as KaniopState, check_api_queryable, create_subscriber,
};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::telemetry;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{Router, get};
use clap::{Parser, crate_authors, crate_description, crate_version};
use kube::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};

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

async fn health() -> impl IntoResponse {
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Args = Args::parse();

    telemetry::init(
        &args.log_filter,
        args.log_format,
        args.tracing_url.as_deref(),
        args.sample_ratio,
    )
    .await?;

    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
    opentelemetry::global::set_meter_provider(provider.clone());
    let meter = opentelemetry::global::meter("kaniop");

    let config = Config::infer().await?;
    let client = new_client_with_metrics(config, &meter).await?;
    let controllers = [
        kaniop_group::controller::CONTROLLER_ID,
        kaniop_operator::kanidm::controller::CONTROLLER_ID,
        kaniop_oauth2::controller::CONTROLLER_ID,
        kaniop_person::controller::CONTROLLER_ID,
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
    );

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
    let person_c = kaniop_person::controller::run(state.clone(), client);

    let app = Router::new()
        .route("/metrics", get(metrics))
        .route("/health", get(health))
        .with_state(state.clone());

    let listener = TcpListener::bind(format!("0.0.0.0:{}", args.port)).await?;
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    tokio::join!(group_c, kanidm_c, oauth2_c, person_c, server).4?;
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
