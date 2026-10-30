//! KanidmRadiusServer controller: shared authorization, preflight and status.
//!
//! Until external identity/token/config rollout orchestration is fully
//! integrated and covered by real authentication E2E, this reconciler does
//! not mutate external Kanidm accounts or Kubernetes workloads. It reports
//! Ready=False rather than falsely marking an API as usable.

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kaniop_k8s_util::error::{Error, Result};
use kaniop_k8s_util::resources::last_transition_time;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Api, Client, ResourceExt};
use serde_json::json;
use tokio::time::Duration;
use tracing::info;

use crate::backoff_reconciler;
use crate::controller::{
    ControllerId, State, check_api_queryable_optional, context::Context, idm_reconcile_interval,
    kanidm::is_resource_watched,
};
use crate::kanidm::radius_crd::KanidmRadiusServer;
use crate::kanidm::radius_preflight::prepare_radius_inputs;
use crate::kanidm::radius_rotation::classify_persisted_state;

pub const CONTROLLER_ID: ControllerId = "radius-server";

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

/// Performs no external mutation. Real reconciliation is intentionally
/// deferred until lifecycle and live authentication gates are implemented.
pub async fn reconcile_radius(
    resource: Arc<KanidmRadiusServer>,
    ctx: Arc<Context<KanidmRadiusServer>>,
) -> Result<(Action, bool)> {
    let interval = idm_reconcile_interval();
    let Some(kanidm) = ctx.get_kanidm(resource.as_ref()) else {
        let changed = update_validation_status(
            resource.as_ref(),
            ctx.client.clone(),
            false,
            "KanidmUnavailable",
            "Referenced Kanidm resource was not found.",
        )
        .await?;
        return Ok((Action::requeue(interval), changed));
    };

    if !is_resource_watched(
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
        return Ok((Action::requeue(interval), changed));
    }
    if !ctx.kanidm_write_allowed(resource.as_ref()) {
        // Restore operations must not be disrupted by RADIUS provisioning.
        return Ok((Action::requeue(Duration::from_secs(5)), false));
    }

    let uri = kanidm
        .spec
        .origin
        .clone()
        .unwrap_or_else(|| format!("https://{}", kanidm.spec.domain));

    let (valid, reason, message) = if classify_persisted_state(
        resource.metadata.uid.as_deref().unwrap_or_default(),
        resource.status.as_ref(),
    )
    .is_err()
    {
        (
            false,
            "RecoveryBlocked",
            "Persisted RADIUS identity state is inconsistent; no provisioning or token cleanup is permitted.",
        )
    } else {
        match prepare_radius_inputs(ctx.client.clone(), resource.as_ref(), &uri).await {
            Ok(_) => (
                true,
                "PreflightPassed",
                "RADIUS input validation passed; provisioning is not yet enabled.",
            ),
            Err(_) => (
                false,
                "PreflightFailed",
                "RADIUS input reference or configuration validation failed; active resources are preserved.",
            ),
        }
    };
    let changed = update_validation_status(
        resource.as_ref(),
        ctx.client.clone(),
        valid,
        reason,
        message,
    )
    .await?;
    Ok((Action::requeue(interval), changed))
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
