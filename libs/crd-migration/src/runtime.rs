use std::{env, time::Duration};

use k8s_openapi::api::{apps::v1::Deployment, core::v1::ConfigMap};
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use tokio::time::sleep;
use tracing::info;

use crate::{
    MigrationError, Result,
    migration::MigrationConfig,
    state::{Phase, get_marker},
};

const FAIL_AFTER_ENV: &str = "KANIOP_MIGRATION_FAIL_AFTER";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Keep failure injection active when Kubernetes restarts a failed migration container.
///
/// The migration state machine persists the completed phase before injecting the failure. Without
/// this check, a restarted container resumes from that marker and advances past the requested
/// failure point.
pub async fn enforce_sticky_fail_injection(
    client: &kube::Client,
    config: &MigrationConfig,
) -> Result<()> {
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
        return Err(MigrationError::InjectedFailure(fail_phase.to_string()));
    }

    Ok(())
}

/// Restore the operator replica count recorded by PreSync before verifying adoption.
///
/// PreSync intentionally scales the operator to zero while replacing the CRD. Helm and Argo CD do
/// not necessarily restore that live replica drift before the PostSync hook runs, so adoption can
/// never complete unless the migrator restores the recorded replica count itself.
pub async fn restore_operator_for_postsync(
    client: &kube::Client,
    config: &MigrationConfig,
) -> Result<()> {
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
                format!(
                    "get deployment {} for PostSync restore",
                    config.operator_deployment
                ),
                Box::new(error),
            )
        })?;
    let current_replicas = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.replicas)
        .unwrap_or(1);

    if current_replicas != original_replicas {
        info!(
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
            info!(
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
            )));
        }

        sleep(POLL_INTERVAL).await;
    }
}
