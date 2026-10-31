//! Crash-safe bootstrap of the first RADIUS API-token generation.
//!
//! Status is the durable non-secret journal and an immutable UID-owned Secret
//! is the bearer journal. The two writes cannot be atomic with Kanidm: after
//! a lost bearer response, reconcile the reserved label and revoke the orphan
//! only after observing that Kubernetes has no consumers of that generation.
//! This module never promotes or revokes a working active generation.

use kanidm_client::KanidmClient;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Patch, PatchParams};
use kube::{Api, Client, Resource, ResourceExt};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use super::radius_crd::KanidmRadiusServer;
use super::radius_generation::config_secret_name;
use super::radius_identity::RadiusIdentity;
use super::radius_identity_reconcile::revoke_drained_orphan_token;
use super::radius_preflight::PreparedRadiusInputs;
use super::radius_rotation::{PersistedRadiusState, TokenGeneration, classify_persisted_state};
use super::radius_secret::{build_config_secret, ensure_config_secret, verify_existing_generation};
use super::radius_token::{candidate_token_label, issue_radius_token};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusBootstrapError {
    #[error("RADIUS bootstrap checkpoint or ownership is invalid")]
    InvalidCheckpoint,
    #[error("RADIUS Kubernetes checkpoint or immutable Secret operation failed")]
    KubernetesUnavailable,
    #[error("RADIUS Kanidm token metadata or issuance failed")]
    KanidmUnavailable,
    #[error("RADIUS bearer was lost; orphan cleanup is blocked by consumers")]
    OrphanBlocked,
    #[error("RADIUS generation Secret and current desired configuration differ")]
    ConfigurationChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// Reservation persisted, or another reconciliation must complete it.
    Requeue,
    /// An immutable generation exists and is eligible for initial rollout.
    Pending(TokenGeneration),
    /// An active generation exists; rotation is managed separately.
    Active(TokenGeneration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReservationDecision {
    Issue,
    Recover,
    RevokeOrphan,
    Block,
}

fn reservation_decision(token_count: usize, secret_exists: bool) -> ReservationDecision {
    match (token_count, secret_exists) {
        (0, false) => ReservationDecision::Issue,
        (1, true) => ReservationDecision::Recover,
        (1, false) => ReservationDecision::RevokeOrphan,
        _ => ReservationDecision::Block,
    }
}

/// Optimistic concurrency on the parent CR prevents two reconciliations from
/// reserving or committing different generations against the same status.
/// Only non-secret IDs and Secret names are patched.
async fn patch_pending(
    client: Client,
    radius: &KanidmRadiusServer,
    checkpoint: Value,
) -> Result<(), RadiusBootstrapError> {
    let namespace = radius.namespace().ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let resource_version = radius
        .metadata
        .resource_version
        .as_deref()
        .ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let api: Api<KanidmRadiusServer> = Api::namespaced(client, &namespace);
    api.patch_status(
        &radius.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&json!({
            "metadata": {"resourceVersion": resource_version},
            "status": checkpoint
        })),
    )
    .await
    .map_err(|_| RadiusBootstrapError::KubernetesUnavailable)?;
    Ok(())
}

async fn get_secret(
    client: Client,
    namespace: &str,
    name: &str,
) -> Result<Option<Secret>, RadiusBootstrapError> {
    let api: Api<Secret> = Api::namespaced(client, namespace);
    match api.get(name).await {
        Ok(secret) => Ok(Some(secret)),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(None),
        Err(_) => Err(RadiusBootstrapError::KubernetesUnavailable),
    }
}

/// A Secret is recoverable only when its UID ownership, immutable contents
/// and bearer are intact. Never use a Secret that happens to share the name.
fn validate_generation_secret(
    secret: &Secret,
    name: &str,
    namespace: &str,
    owner: &k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
    inputs: &PreparedRadiusInputs,
) -> Result<(), RadiusBootstrapError> {
    let bearer = secret
        .data
        .as_ref()
        .and_then(|data| data.get("auth_token"))
        .ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let bearer = String::from_utf8(bearer.0.clone())
        .map_err(|_| RadiusBootstrapError::InvalidCheckpoint)?;
    let config = inputs
        .config_with_token(bearer)
        .map_err(|_| RadiusBootstrapError::ConfigurationChanged)?;
    let expected = build_config_secret(name, namespace, owner, &config)
        .map_err(|_| RadiusBootstrapError::ConfigurationChanged)?;
    verify_existing_generation(secret, &expected, owner)
        .map_err(|_| RadiusBootstrapError::ConfigurationChanged)
}

/// Persist a first-generation reservation, recover an interrupted issuance,
/// or return an existing pending/active checkpoint. Each call is idempotent.
///
/// Call only after the finalizer, authorization, restore guard, preflight and
/// UID-owned Kanidm identity have been established.
pub async fn ensure_initial_generation(
    kube_client: Client,
    idm_client: &KanidmClient,
    radius: &KanidmRadiusServer,
    identity: &RadiusIdentity,
    inputs: &PreparedRadiusInputs,
) -> Result<BootstrapOutcome, RadiusBootstrapError> {
    let uid = radius
        .metadata
        .uid
        .as_deref()
        .ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let namespace = radius.namespace().ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let owner = radius
        .controller_owner_ref(&())
        .ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
    let state = classify_persisted_state(uid, radius.status.as_ref())
        .map_err(|_| RadiusBootstrapError::InvalidCheckpoint)?;

    match state {
        PersistedRadiusState::Empty => {
            patch_pending(
                kube_client,
                radius,
                json!({"pendingTokenGeneration": Uuid::new_v4().to_string()}),
            )
            .await?;
            Ok(BootstrapOutcome::Requeue)
        }
        PersistedRadiusState::Active(active) => Ok(BootstrapOutcome::Active(active)),
        PersistedRadiusState::Pending { active, pending } => {
            // Do not modify an active workload while a rotation is pending.
            if active.is_some() {
                return Ok(BootstrapOutcome::Requeue);
            }
            let secret = get_secret(kube_client, &namespace, &pending.config_secret)
                .await?
                .ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
            validate_generation_secret(
                &secret,
                &pending.config_secret,
                &namespace,
                &owner,
                inputs,
            )?;
            let label = candidate_token_label(&pending.revision)
                .map_err(|_| RadiusBootstrapError::InvalidCheckpoint)?;
            let tokens = idm_client
                .idm_service_account_list_api_token(&identity.account_name)
                .await
                .map_err(|_| RadiusBootstrapError::KanidmUnavailable)?;
            if tokens.iter().filter(|token| token.label == label
                && token.token_id.to_string() == pending.token_id).count() != 1
            {
                return Err(RadiusBootstrapError::InvalidCheckpoint);
            }
            Ok(BootstrapOutcome::Pending(pending))
        }
        PersistedRadiusState::Reserved { active, generation } => {
            if active.is_some() {
                return Ok(BootstrapOutcome::Requeue);
            }
            let name = config_secret_name(uid, &generation)
                .map_err(|_| RadiusBootstrapError::InvalidCheckpoint)?;
            let secret = get_secret(kube_client.clone(), &namespace, &name).await?;
            let label = candidate_token_label(&generation)
                .map_err(|_| RadiusBootstrapError::InvalidCheckpoint)?;
            let tokens = idm_client
                .idm_service_account_list_api_token(&identity.account_name)
                .await
                .map_err(|_| RadiusBootstrapError::KanidmUnavailable)?;
            let matching: Vec<_> = tokens.iter().filter(|token| token.label == label).collect();
            match reservation_decision(matching.len(), secret.is_some()) {
                ReservationDecision::Issue => {
                    let issued = issue_radius_token(idm_client, &identity.account_name, &generation)
                        .await
                        .map_err(|_| RadiusBootstrapError::KanidmUnavailable)?;
                    let config = inputs
                        .config_with_token(issued.bearer)
                        .map_err(|_| RadiusBootstrapError::ConfigurationChanged)?;
                    let desired = build_config_secret(&name, &namespace, &owner, &config)
                        .map_err(|_| RadiusBootstrapError::ConfigurationChanged)?;
                    ensure_config_secret(kube_client.clone(), &desired, &owner)
                        .await
                        .map_err(|_| RadiusBootstrapError::KubernetesUnavailable)?;
                    patch_pending(
                        kube_client,
                        radius,
                        json!({
                            "pendingTokenId": issued.token_id.to_string(),
                            "pendingConfigSecret": name
                        }),
                    )
                    .await?;
                    Ok(BootstrapOutcome::Requeue)
                }
                ReservationDecision::Recover => {
                    let secret = secret.ok_or(RadiusBootstrapError::InvalidCheckpoint)?;
                    validate_generation_secret(&secret, &name, &namespace, &owner, inputs)?;
                    patch_pending(
                        kube_client,
                        radius,
                        json!({
                            "pendingTokenId": matching[0].token_id.to_string(),
                            "pendingConfigSecret": name
                        }),
                    )
                    .await?;
                    Ok(BootstrapOutcome::Requeue)
                }
                ReservationDecision::RevokeOrphan => {
                    let revoked = revoke_drained_orphan_token(
                        idm_client,
                        kube_client.clone(),
                        radius,
                        &generation,
                    )
                    .await
                    .map_err(|_| RadiusBootstrapError::OrphanBlocked)?;
                    if !revoked {
                        return Err(RadiusBootstrapError::OrphanBlocked);
                    }
                    patch_pending(
                        kube_client,
                        radius,
                        json!({"pendingTokenGeneration": null}),
                    )
                    .await?;
                    Ok(BootstrapOutcome::Requeue)
                }
                ReservationDecision::Block => Err(RadiusBootstrapError::InvalidCheckpoint),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_recovery_never_mints_again_after_a_lost_bearer() {
        assert_eq!(reservation_decision(0, false), ReservationDecision::Issue);
        assert_eq!(reservation_decision(1, false), ReservationDecision::RevokeOrphan);
        assert_eq!(reservation_decision(1, true), ReservationDecision::Recover);
        assert_eq!(reservation_decision(0, true), ReservationDecision::Block);
        assert_eq!(reservation_decision(2, false), ReservationDecision::Block);
        assert_eq!(reservation_decision(2, true), ReservationDecision::Block);
    }
}
