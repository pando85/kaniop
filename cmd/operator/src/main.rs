use actix_web::{
    get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use kaniop_k8s_util::client::new_client_with_metrics;
use kaniop_operator::controller::State;
use kaniop_operator::telemetry;

use clap::{crate_authors, crate_description, crate_version, Parser};
use kube::Config;
use prometheus_client::registry::Registry;

#[get("/metrics")]
async fn metrics(c: Data<State>, _req: HttpRequest) -> impl Responder {
    match c.metrics() {
        Ok(metrics) => HttpResponse::Ok()
            .content_type("application/openmetrics-text; version=1.0.0; charset=utf-8")
            .body(metrics),
        Err(e) => {
            tracing::error!("Failed to get metrics: {:?}", e);
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
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
    port: u32,

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

    let mut registry = Registry::with_prefix("kaniop");
    let config = Config::infer().await?;
    let client = new_client_with_metrics(config, &mut registry).await?;
    let controllers = [kaniop_kanidm::controller::CONTROLLER_ID];
    let state = State::new(registry, &controllers);

    let controller = kaniop_kanidm::controller::run(state.clone(), client);

    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(health)
            .service(metrics)
    })
    .bind(format!("0.0.0.0:{}", args.port))?
    .shutdown_timeout(5);

    // Both runtimes implements graceful shutdown, so poll until both are done
    tokio::join!(controller, server.run()).1?;
    Ok(())
}
