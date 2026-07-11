use clap::{Parser, Subcommand, crate_authors, crate_description, crate_version};
use kaniop_crd_migration::migration::{MigrationConfig, run_postsync, run_presync};
use rustls::crypto::aws_lc_rs::default_provider;

#[derive(Parser, Debug)]
#[command(
    name = "kaniop-crd-migrator",
    about = crate_description!(),
    version = crate_version!(),
    author = crate_authors!("\n"),
)]
struct Args {
    #[arg(long, default_value = "info", env)]
    log_filter: String,

    #[arg(long, default_value = "kaniop", env)]
    namespace: String,

    #[arg(long, default_value = "kaniop", env)]
    operator_namespace: String,

    #[arg(long, default_value = "kaniop", env)]
    operator_deployment: String,

    #[arg(long, default_value = "kaniop-person-crd-migration", env)]
    marker_name: String,

    #[arg(long, default_value_t = 300, env)]
    timeout_seconds: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    MigratePersonAccount,
    VerifyPersonAccount,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    default_provider().install_default().unwrap();

    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_filter)),
        )
        .init();

    let config = MigrationConfig {
        namespace: args.namespace,
        operator_namespace: args.operator_namespace,
        operator_deployment: args.operator_deployment,
        marker_name: args.marker_name,
        timeout: std::time::Duration::from_secs(args.timeout_seconds),
    };

    let client = kube::Client::try_default().await?;

    match args.command {
        Command::MigratePersonAccount => {
            run_presync(&client, &config).await?;
        }
        Command::VerifyPersonAccount => {
            let result = run_postsync(&client, &config).await?;
            if kaniop_crd_migration::verify::adoption_verification_passed(&result) {
                tracing::info!(
                    source_count = result.source_count,
                    restored_count = result.restored_count,
                    "PostSync verification passed"
                );
            } else {
                tracing::error!(
                    source_count = result.source_count,
                    restored_count = result.restored_count,
                    missing_restorations = ?result.missing_restorations,
                    checksum_mismatches = ?result.checksum_mismatches,
                    missing_finalizers = ?result.missing_finalizers,
                    missing_exists = ?result.missing_exists,
                    "PostSync verification failed"
                );
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
