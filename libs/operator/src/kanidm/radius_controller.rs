//! KanidmRadiusServer controller: declarative preflight, identity and finalizer.
//!
//! Provisioning is staged: a finalizer is installed before any external
//! identity mutation. Token/config/workload rollout remains disabled until
//! the crash-recovery and live-authentication gates are implemented. The
//! controller never reports Ready=true without a working RADIUS endpoint.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::resources::last_transition_time;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::runtime::watcher;
use kube::{Api, Client, Resource, ResourceExt};
use serde_json::json;
use tokio::time::Duration;
use tracing::info;

use crate::backoff_reconciler;
use crate::controller::{
    ControllerId, State, check_api_queryable_optional,
    context::{Context, IdmClientContext},
    idm_reconcile_interval,
    kanidm::is_resource_watched,
};
use crate::kanidm::radius_apply::ensure_radius_workload;
use crate::kanidm::radius_bootstrap::{BootstrapOutcome, ensure_initial_generation};
use crate::kanidm::radius_cleanup::{DrainState, delete_owned_radius_workload};
use crate::kanidm::radius_crd::KanidmRadiusServer;
use crate::kanidm::radius_identity_reconcile::{
    cleanup_radius_identity_after_drain, ensure_radius_identity, revoke_drained_orphan_token,
};
use crate::kanidm::radius_image::resolve_radius_image;
use crate::kanidm::radius_preflight::prepare_radius_inputs;
use crate::kanidm::radius_rotation::classify_persisted_state;
use crate::kanidm::radius_workload::workload_for_generation;

pub const CONTROLLER_ID: ControllerId = "radius-server";
pub const RADIUS_FINALIZER: &str = "kanidmradiusservers.kaniop.rs/finalizer";

/// Patch only when status meaningfully changes; preserve transition time and
/// avoid requeue storms. No Secret data, tokens or confidential error bodies
/// may enter status.
async fn update_validation_status(
    resource: &KanidmRadiusServer,
    client: Client,
    valid: bool,
    reason: &str,
    message: &str,
) -> Result<bool> {
    let namespace = resource
        .namespace()
        .ok_or_else(|| Error::MissingData("RADIUS namespace is absent".into()))?;
    let current = resource
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref());
    let value = if valid { "True" } else { "False" };
    let condition = Condition {
        type_: "ConfigurationReady".into(),
        status: value.into(),
        reason: reason.into(),
        message: message.into(),
        last_transition_time: last_transition_time(current, "ConfigurationReady", value, reason),
        observed_generation: resource.metadata.generation,
    };
    let already_reported = resource.status.as_ref().is_some_and(|status| {
        !status.ready && status.conditions.as_deref() == Some(&[condition.clone()][..])
    });
    if already_reported {
        return Ok(false);
    }
    let api: Api<KanidmRadiusServer> = Api::namespaced(client, &namespace);
    api.patch_status(
        &resource.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&json!({
            "status": {
                "ready": false,
                "conditions": [condition],
                "observedGeneration": resource.metadata.generation
            }
        })),
    )
    .await
    .map_err(|e| {
        Error::kube_error(
            "patch",
            "KanidmRadiusServer/status",
            &namespace,
            resource.name_any(),
            e,
        )
    })?;
    Ok(true)
}

/// Reconcile only after kube-rs has durably attached the finalizer.
/// Validation runs before creating any external Kanidm identity, and the
/// last-good runtime remains untouched on an invalid desired configuration.
async fn apply_radius(
    resource: &KanidmRadiusServer,
    ctx: &Context<KanidmRadiusServer>,
    idm_client: &kanidm_client::KanidmClient,
) -> Result<(Action, bool)> {
    let interval = idm_reconcile_interval();
    let Some(kanidm) = ctx.get_kanidm(resource) else {
        let changed = update_validation_status(
            resource,
            ctx.client.clone(),
            false,
            "KanidmUnavailable",
            "Referenced Kanidm resource was not found.",
        )
        .await?;
        return Ok((Action::requeue(interval), changed));
    };
    if !is_resource_watched(resource, &kanidm, &ctx.namespace_store, &ctx.client).await {
        let changed = update_validation_status(
            resource,
            ctx.client.clone(),
            false,
            "NamespaceNotAuthorized",
            "Configure radiusServerNamespaceSelector on the referenced Kanidm.",
        )
        .await?;
        return Ok((Action::requeue(interval), changed));
    }

    if classify_persisted_state(
        resource.metadata.uid.as_deref().unwrap_or_default(),
        resource.status.as_ref(),
    )
    .is_err()
    {
        let changed = update_validation_status(
            resource,
            ctx.client.clone(),
            false,
            "RecoveryBlocked",
            "Persisted RADIUS identity state is inconsistent; no provisioning or token cleanup is permitted.",
        )
        .await?;
        return Ok((Action::requeue(interval), changed));
    }
    let uri = kanidm
        .spec
        .origin
        .clone()
        .unwrap_or_else(|| format!("https://{}", kanidm.spec.domain));
    let inputs = match prepare_radius_inputs(ctx.client.clone(), resource, &uri).await {
        Ok(inputs) => inputs,
        Err(_) => {
            let changed = update_validation_status(
                resource,
                ctx.client.clone(),
                false,
                "PreflightFailed",
                "RADIUS input reference or configuration validation failed; active resources are preserved.",
            )
            .await?;
            return Ok((Action::requeue(interval), changed));
        }
    };
    let image = match resolve_radius_image(&kanidm.spec.image, resource.spec.image.as_deref()) {
        Ok(image) => image,
        Err(_) => {
            let changed = update_validation_status(
                resource,
                ctx.client.clone(),
                false,
                "ImageUnavailable",
                "The RADIUS image must be a verified release or explicitly configured.",
            )
            .await?;
            return Ok((Action::requeue(interval), changed));
        }
    };

    // The finalizer is already installed. First ensure UID-owned identity,
    // then advance the persisted token/Secret journal by at most one step.
    // No pending generation is promoted or rolled into an existing workload
    // without the separate real-authentication verification gate.
    let (valid, reason, message) = match ensure_radius_identity(idm_client, resource).await {
        Ok(identity) => {
            match ensure_initial_generation(
                ctx.client.clone(),
                idm_client,
                resource,
                &identity,
                &inputs,
            )
            .await
            {
                Ok(BootstrapOutcome::Requeue) => (
                    true,
                    "BootstrapProgress",
                    "RADIUS identity and initial credential checkpoint reconciled; runtime rollout is not yet enabled.",
                ),
                Ok(BootstrapOutcome::Pending(generation)) => {
                    match workload_for_generation(resource, image, &generation) {
                        Ok(workload) => {
                            match ensure_radius_workload(ctx.client.clone(), &workload).await {
                                Ok(()) => (
                                    true,
                                    "RolloutPendingAuth",
                                    "Initial RADIUS workload is deployed; a real authentication probe is required before reporting Ready.",
                                ),
                                Err(_) => (
                                    false,
                                    "WorkloadBlocked",
                                    "RADIUS Deployment or Service reconciliation failed; the persisted credential generation is preserved.",
                                ),
                            }
                        }
                        Err(_) => (
                            false,
                            "WorkloadInvalid",
                            "RADIUS workload configuration is invalid; existing credentials and workloads are preserved.",
                        ),
                    }
                }
                Ok(BootstrapOutcome::Active(_)) => (
                    true,
                    "ActiveGeneration",
                    "An active RADIUS generation is recorded; runtime health verification is not yet enabled.",
                ),
                Err(_) => (
                    false,
                    "BootstrapBlocked",
                    "RADIUS credential checkpoint reconciliation is blocked; existing credentials and workloads are preserved.",
                ),
            }
        }
        Err(_) => (
            false,
            "IdentityUnavailable",
            "RADIUS identity reconciliation failed or ownership was rejected; no runtime rollout was attempted.",
        ),
    };
    let changed = update_validation_status(
        resource,
        ctx.client.clone(),
        valid,
        reason,
        message,
    )
    .await?;
    Ok((Action::requeue(interval), changed))
}

/// Finalization is a separate declarative path: input references and current
/// namespace selectors must not prevent removal of an already-owned identity.
/// Restore guards are checked by the outer reconciler before any mutation.
async fn cleanup_radius(
    resource: &KanidmRadiusServer,
    ctx: &Context<KanidmRadiusServer>,
    idm_client: &kanidm_client::KanidmClient,
) -> Result<Action> {
    let namespace = resource
        .namespace()
        .ok_or_else(|| Error::MissingData("RADIUS namespace is absent".into()))?;
    let owner = resource
        .controller_owner_ref(&())
        .ok_or_else(|| Error::MissingData("RADIUS owner UID is absent".into()))?;
    let drain = delete_owned_radius_workload(
        ctx.client.clone(),
        &namespace,
        &resource.name_any(),
        &owner,
    )
    .await
    .map_err(|_| Error::MissingData("RADIUS workload cleanup is blocked".into()))?;
    if drain != DrainState::Drained {
        // Returning Ok from Finalizer::Cleanup would remove the finalizer.
        // A pending foreground deletion MUST instead retain it for a retry.
        return Err(Error::MissingData(
            "RADIUS pods are still draining; retaining finalizer".into(),
        ));
    }
    // A bearer minted immediately before a crash may not have a recorded
    // token ID. After workload drain, recover that exact reserved generation
    // by UID-bound account and label before finalizing the remaining tokens.
    if let Some(generation) = resource
        .status
        .as_ref()
        .and_then(|status| status.pending_token_generation.as_deref())
        && resource
            .status
            .as_ref()
            .is_some_and(|status| status.pending_token_id.is_none())
    {
        revoke_drained_orphan_token(
            idm_client,
            ctx.client.clone(),
            resource,
            generation,
        )
        .await
        .map_err(|_| Error::MissingData("RADIUS orphan-token cleanup is blocked".into()))?;
    }
    cleanup_radius_identity_after_drain(idm_client, resource)
        .await
        .map_err(|_| Error::MissingData("RADIUS identity cleanup is blocked".into()))?;
    Ok(Action::await_change())
}

/// Attach a finalizer before mutating Kanidm, and retain it until Kubernetes
/// consumers are drained and the UID-owned identity has been cleaned up.
pub async fn reconcile_radius(
    resource: Arc<KanidmRadiusServer>,
    ctx: Arc<Context<KanidmRadiusServer>>,
) -> Result<(Action, bool)> {
    if !ctx.kanidm_write_allowed(resource.as_ref()) {
        return Ok((Action::requeue(Duration::from_secs(5)), false));
    }
    // Avoid installing an unserviceable finalizer when the target Kanidm CR
    // is not yet present. Once installed, deletion still goes through cleanup.
    if ctx.get_kanidm(resource.as_ref()).is_none()
        && resource.metadata.deletion_timestamp.is_none()
        && !resource.finalizers().iter().any(|f| f == RADIUS_FINALIZER)
    {
        let changed = update_validation_status(
            resource.as_ref(),
            ctx.client.clone(),
            false,
            "KanidmUnavailable",
            "Referenced Kanidm resource was not found.",
        )
        .await?;
        return Ok((Action::requeue(idm_reconcile_interval()), changed));
    }

    // Do not attach a finalizer to a newly created CR that cannot be
    // authorized. An existing finalizer must still run its cleanup path
    // after a selector change or during deletion.
    if resource.metadata.deletion_timestamp.is_none()
        && !resource.finalizers().iter().any(|f| f == RADIUS_FINALIZER)
        && let Some(kanidm) = ctx.get_kanidm(resource.as_ref())
        && !is_resource_watched(
            resource.as_ref(),
            &kanidm,
            &ctx.namespace_store,
            &ctx.client,
        )
        .await
    {
        let changed = update_validation_status(
            resource.as_ref(),
            ctx.client.clone(),
            false,
            "NamespaceNotAuthorized",
            "Configure radiusServerNamespaceSelector on the referenced Kanidm.",
        )
        .await?;
        return Ok((Action::requeue(idm_reconcile_interval()), changed));
    }

    let namespace = resource
        .namespace()
        .ok_or_else(|| Error::MissingData("RADIUS namespace is absent".into()))?;
    let api: Api<KanidmRadiusServer> = Api::namespaced(ctx.client.clone(), &namespace);
    let idm_client = ctx.get_idm_client(resource.as_ref()).await?;
    let changed = Arc::new(AtomicBool::new(false));
    let changed_outcome = changed.clone();
    let action = finalizer(&api, RADIUS_FINALIZER, resource, move |event| {
        let ctx = ctx.clone();
        let idm_client = idm_client.clone();
        let changed_outcome = changed_outcome.clone();
        async move {
            match event {
                Finalizer::Apply(radius) => {
                    let (action, did_change) =
                        apply_radius(&radius, &ctx, &idm_client).await?;
                    changed_outcome.store(did_change, Ordering::Relaxed);
                    Ok(action)
                }
                Finalizer::Cleanup(radius) => cleanup_radius(&radius, &ctx, &idm_client).await,
            }
        }
    })
    .await
    .map_err(|error| {
        Error::FinalizerError(
            "RADIUS finalizer reconciliation failed".into(),
            Box::new(error),
        )
    })?;
    Ok((action, changed.load(Ordering::Relaxed)))
}

pub async fn run(state: State, client: Client) {
    let Some(api) = check_api_queryable_optional::<KanidmRadiusServer>(client.clone()).await else {
        info!(
            controller = CONTROLLER_ID,
            "RADIUS CRD unavailable; controller disabled"
        );
        return;
    };
    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));
    info!(controller = CONTROLLER_ID, "starting RADIUS controller");
    Controller::new(api, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_radius),
            |_obj, _error: &Error, _ctx| unreachable!(),
            ctx,
        )
        .filter_map(|res| async move { std::result::Result::ok(res) })
        .for_each(|_| futures::future::ready(()))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_id_is_stable_for_metrics() {
        assert_eq!(CONTROLLER_ID, "radius-server");
    }
}
