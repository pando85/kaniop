mod checksum;
mod commands;
mod s3;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kaniop-data-mover", about = "S3 data mover for Kanidm backups")]
struct Cli {
    #[arg(long, default_value = "info", env = "DATA_MOVER_LOG_FILTER")]
    log_filter: String,

    #[arg(long, default_value = "json", env)]
    log_format: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Upload {
        #[arg(
            long,
            env = "OPERATION_DOC_PATH",
            default_value = "/run/kaniop/operation.json"
        )]
        operation_doc: String,
    },
    Download {
        #[arg(
            long,
            env = "OPERATION_DOC_PATH",
            default_value = "/run/kaniop/operation.json"
        )]
        operation_doc: String,
    },
    DeletePlan {
        #[arg(
            long,
            env = "OPERATION_DOC_PATH",
            default_value = "/run/kaniop/operation.json"
        )]
        operation_doc: String,
    },
    Discover {
        #[arg(
            long,
            env = "OPERATION_DOC_PATH",
            default_value = "/run/kaniop/operation.json"
        )]
        operation_doc: String,
    },
    Check {
        #[arg(
            long,
            env = "OPERATION_DOC_PATH",
            default_value = "/run/kaniop/operation.json"
        )]
        operation_doc: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let filter = EnvFilter::try_new(&cli.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));
    match cli.log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .init();
        }
        _ => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }

    let exit_code = match cli.command {
        Commands::Upload { operation_doc } => commands::upload::run(&operation_doc).await,
        Commands::Download { operation_doc } => commands::download::run(&operation_doc).await,
        Commands::DeletePlan { operation_doc } => commands::delete_plan::run(&operation_doc).await,
        Commands::Discover { operation_doc } => commands::discover::run(&operation_doc).await,
        Commands::Check { operation_doc } => commands::check::run(&operation_doc).await,
    };

    match exit_code {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code as u8),
    }
}
