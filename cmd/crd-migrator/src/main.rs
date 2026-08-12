use std::{env, time::Duration};

use clap::{Parser, Subcommand, crate_authors, crate_description, crate_version};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::ConfigMap};
use kaniop_crd_migration::{
    MigrationError,
    migration::{MigrationConfig, run_postsync, run_presync},
    state::{Phase, get_marker},
};
use kube::{Api, api::{Patch, PatchParams}};
use rustls::crypto::aws_lc_rs::default_provider;
use tokio::time::sleep;

const FAIL_AFTER_ENV: &str = "KANIOP_MIGRATION_FAIL_AFTER";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

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

async fn enforce_sticky_fail_injection(
    client: &kube::Client,
    config: &MigrationConfig,
) -> anyhow::Result<()> {
    let Some(fail_after) = env::var(FAIL_AFTER_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let fail_phase: Phase = fail_after.parse()?;
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);
    let Some(marker) = get_marker(&marker_api, &config.marker_name).await? else {
        return Ok(());
    };

    if marker.phase == fail_phase {
        return Err(MigrationError::InjectedFailure(fail_phase.to_string()).into());
    }

    Ok(())
}

async fn restore_operator_for_postsync(
    client: &kube::Client,
    config: &MigrationConfig,
) -> anyhow::Result<()> {
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);
    let Some(marker) = get_marker(&marker_api, &config.marker_name).await? else {
        return Ok(());
    };

    if marker.phase < Phase::Verified || marker.phase >= Phase::Completed {
        return Ok(());
    }

    let Some(original_replicas) = marker.original_replicas else {
        return Ok(());
    };

    let deployment_api: Api<Deployment> =
        Api::namespaced(client.clone(), &config.operator_namespace);
    let deployment = deployment_api
        .get(&config.operator_deployment)
        .await
        .map_err(|error| {
            MigrationError::Kube(
                format!("get deployment {} for PostSync restore", config.operator_deployment),
                Box::new(error),
            )
        })?;
    let current_replicas = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.replicas)
        .unwrap_or(1);

    if current_replicas != original_replicas {
        tracing::info!(
            deployment = %config.operator_deployment,
            replicas = original_replicas,
            "restoring operator replicas before PostSync adoption verification"
        );
        let patch = serde_json::json!({
            "spec": {
                "replicas": original_replicas
            }
        });
        deployment_api
            .patch(
                &config.operator_deployment,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|error| {
                MigrationError::Kube(
                    format!(
                        "restore deployment {} to {original_replicas} replicas",
                        config.operator_deployment
                    ),
                    Box::new(error),
                )
            })?;
    }

    if original_replicas == 0 {
        return Ok(());
    }

    let start = std::time::Instant::now();
    loop {
        let deployment = deployment_api
            .get(&config.operator_deployment)
            .await
            .map_err(|error| {
                MigrationError::Kube(
                    format!(
                        "get deployment {} while waiting for PostSync restore",
                        config.operator_deployment
                    ),
                    Box::new(error),
                )
            })?;
        let status = deployment.status.as_ref();
        let ready = status.and_then(|status| status.ready_replicas).unwrap_or(0);
        let available = status
            .and_then(|status| status.available_replicas)
            .unwrap_or(0);

        if ready >= original_replicas && available >= original_replicas {
            tracing::info!(
                deployment = %config.operator_deployment,
                replicas = original_replicas,
                "operator restored for PostSync adoption verification"
            );
            return Ok(());
        }

        if start.elapsed() > config.timeout {
            return Err(MigrationError::Timeout(format!(
                "deployment {} did not restore to {original_replicas} ready replicas within timeout",
                config.operator_deployment
            ))
            .into());
        }

        sleep(POLL_INTERVAL).await;
    }
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
            enforce_sticky_fail_injection(&client, &config).await?;
            run_presync(&client, &config).await?;
        }
        Command::VerifyPersonAccount => {
            restore_operator_for_postsync(&client, &config).await?;
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
