use std::{env, time::Duration};

use jiff::Timestamp;
use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::ConfigMap},
    apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
};
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use tokio::time::sleep;
use tracing::info;

use crate::{
    CORRECTED_CRD_NAME, LEGACY_CRD_NAME, MigrationError, Result,
    crd::get_crd,
    migration::MigrationConfig,
    state::{MigrationMarker, Phase, create_or_update_marker, get_marker},
};

const FAIL_AFTER_ENV: &str = "KANIOP_MIGRATION_FAIL_AFTER";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Persist the operator's desired replica count before PreSync can reach a failure checkpoint.
///
/// A migration retry must know how many operator replicas to restore after replacing the CRD. The
/// destructive state machine used to record this only while scaling the Deployment down, after the
/// `BackedUp` checkpoint. A failure at that checkpoint therefore left no restore target and
/// PostSync could wait forever for adoption while the operator remained stopped.
pub async fn persist_original_operator_replicas_for_presync(
    client: &kube::Client,
    config: &MigrationConfig,
) -> Result<()> {
    let marker_api: Api<ConfigMap> = Api::namespaced(client.clone(), &config.namespace);

    if let Some(mut marker) = get_marker(&marker_api, &config.marker_name).await? {
        if marker.original_replicas.is_some() {
            return Ok(());
        }

        // Fresh installs never stop the operator, so their Verified/Completed zero-source marker
        // legitimately has no restore target. Subsequent upgrades must keep that state a no-op.
        if marker.phase >= Phase::Verified
            && marker.source_count == 0
            && marker.restored_count == 0
        {
            return Ok(());
        }

        if marker.phase > Phase::BackedUp {
            return Err(MigrationError::State(format!(
                "migration marker is at phase {} without originalReplicas; cannot safely restore operator",
                marker.phase
            )));
        }

        let deployment_api: Api<Deployment> =
            Api::namespaced(client.clone(), &config.operator_namespace);
        let deployment = deployment_api
            .get(&config.operator_deployment)
            .await
            .map_err(|error| {
                MigrationError::Kube(
                    format!(
                        "get deployment {} while repairing migration marker",
                        config.operator_deployment
                    ),
                    Box::new(error),
                )
            })?;
        let replicas = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.replicas)
            .unwrap_or(1);

        if marker.phase == Phase::BackedUp && replicas == 0 {
            return Err(MigrationError::State(format!(
                "migration marker is at BackedUp without originalReplicas and deployment {} is already scaled to zero; original replica count is ambiguous",
                config.operator_deployment
            )));
        }

        marker.original_replicas = Some(replicas);
        marker.updated_at = Timestamp::now().to_string();
        create_or_update_marker(&marker_api, &marker, &config.marker_name, &config.namespace)
            .await?;
        info!(
            deployment = %config.operator_deployment,
            replicas,
            phase = %marker.phase,
            "persisted missing original operator replica count"
        );
        return Ok(());
    }

    // Do not create migration state on fresh installs or already-migrated clusters. Creating a
    // marker here is only valid for the legacy-only state, where run_presync would otherwise start
    // a new migration immediately afterwards.
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let legacy_crd = get_crd(&crd_api, LEGACY_CRD_NAME).await?;
    let corrected_crd = get_crd(&crd_api, CORRECTED_CRD_NAME).await?;
    if legacy_crd.is_none() || corrected_crd.is_some() {
        return Ok(());
    }

    let deployment_api: Api<Deployment> =
        Api::namespaced(client.clone(), &config.operator_namespace);
    let deployment = deployment_api
        .get(&config.operator_deployment)
        .await
        .map_err(|error| {
            MigrationError::Kube(
                format!(
                    "get deployment {} before starting migration",
                    config.operator_deployment
                ),
                Box::new(error),
            )
        })?;
    let replicas = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.replicas)
        .unwrap_or(1);

    let now = Timestamp::now().to_string();
    let mut marker = MigrationMarker::new(&now);
    marker.original_replicas = Some(replicas);
    create_or_update_marker(&marker_api, &marker, &config.marker_name, &config.namespace).await?;
    info!(
        deployment = %config.operator_deployment,
        replicas,
        "persisted original operator replica count before migration"
    );

    Ok(())
}

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
        // Fresh installs create a Verified zero-source marker without ever stopping the operator,
        // so there is intentionally no replica count to restore. Destructive migrations are
        // required to persist a restore target before their first checkpoint.
        if marker.source_count == 0 && marker.restored_count == 0 {
            info!("PostSync has a zero-source marker; no operator restore is required");
            return Ok(());
        }

        return Err(MigrationError::State(format!(
            "migration marker at phase {} is missing originalReplicas; refusing to wait for PostSync adoption without a restore target",
            marker.phase
        )));
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
