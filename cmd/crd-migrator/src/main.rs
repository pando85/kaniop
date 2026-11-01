mod apply_crds;

use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, crate_authors, crate_description, crate_version};
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

    #[arg(long, default_value_t = 300, env)]
    timeout_seconds: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    ApplyCrds,
}

#[tokio::main]
async fn main() -> Result<()> {
    default_provider().install_default().unwrap();

    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_filter)),
        )
        .init();

    match args.command {
        Command::ApplyCrds => {
            apply_crds::apply_crds(Duration::from_secs(args.timeout_seconds)).await?;
        }
    }

    Ok(())
}
