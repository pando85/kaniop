//! Dedicated UID-bound Kanidm identity for RADIUS.
//!
//! Each RADIUS resource owns exactly one Kanidm service account, joined to
//! the built-in idm_radius_servers group *additively*. We deliberately never
//! use group_set_members, which could revoke unrelated RADIUS instances.
//! Call only after namespace authorization, restore guard and input preflight.

use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::v1::Entry;
use kaniop_k8s_util::types::normalize_spn;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::api::ListParams;
use kube::{Api, Client, Resource, ResourceExt};
use thiserror::Error;

use super::radius_crd::{KanidmRadiusServer, KanidmRadiusServerStatus};
use super::radius_cleanup::verify_owner;
use super::radius_generation::config_secret_name;
use super::radius_identity::{RadiusIdentity, RadiusIdentityError};
use super::radius_token::candidate_token_label;

const RADIUS_GROUP: &str = "idm_radius_servers";
const ENTRY_MANAGER: &str = "idm_admin";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusIdentityReconcileError {
    #[error("RADIUS resource has no namespace or immutable UID")]
    MissingMetadata,
    #[error("a Kanidm service account already exists without the expected RADIUS owner marker")]
    OwnershipConflict,
    #[error("Kanidm RADIUS identity operation failed")]
    KanidmUnavailable,
    #[error("RADIUS service account has unknown API tokens and cannot be deleted safely")]
    ForeignTokensPresent,
    #[error("RADIUS Kubernetes state cannot be observed safely")]
    KubernetesUnavailable,
}

/// Check the authoritative display-name ownership marker. Never update an
/// existing account to *make it match*: that would be silent account adoption.
fn owned_entry(entry: &Entry, identity: &RadiusIdentity) -> bool {
    entry
        .attrs
        .get("displayname")
        .is_some_and(|names| names.len() == 1 && identity.matches_existing_display_name(&names[0]))
}

fn conflict(error: &ClientError) -> bool {
    matches!(error, ClientError::Http(status, _, _) if status.as_u16() == 409)
}

fn identity_for(
    radius: &KanidmRadiusServer,
) -> Result<RadiusIdentity, RadiusIdentityReconcileError> {
    let namespace = radius
        .namespace()
        .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
    let uid = radius
        .metadata
        .uid
        .as_deref()
        .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
    RadiusIdentity::new(&namespace, &radius.name_any(), uid)
        .map_err(|_: RadiusIdentityError| RadiusIdentityReconcileError::MissingMetadata)
}

/// A syntactically valid label is not ownership evidence. Only the exact
/// token ID + generation recorded in this CR may be revoked on finalization.
/// A token minted just before a crash but not persisted must block cleanup.
fn is_recorded_radius_token(
    status: Option<&KanidmRadiusServerStatus>,
    token_id: &str,
    label: &str,
) -> bool {
    let Some(status) = status else {
        return false;
    };
    [
        (
            status.active_token_id.as_deref(),
            status.active_token_generation.as_deref(),
        ),
        (
            status.pending_token_id.as_deref(),
            status.pending_token_generation.as_deref(),
        ),
    ]
    .into_iter()
    .any(|(id, generation)| {
        matches!((id, generation), (Some(id), Some(generation))
            if id == token_id
                && candidate_token_label(generation).ok().as_deref() == Some(label))
    })
}

/// A stranded bearer is eligible for cleanup only while its exact generation
/// is pending (reserved or issued), never active. A missing configuration
/// Secret and absence of consumers are checked against Kubernetes separately.
fn pending_orphan(status: Option<&KanidmRadiusServerStatus>, generation: &str) -> bool {
    status.is_some_and(|status| {
        status.pending_token_generation.as_deref() == Some(generation)
            && status.active_token_generation.as_deref() != Some(generation)
            && (status.pending_token_id.is_none() == status.pending_config_secret.is_none())
    })
}

/// For issued generations the token ID, not merely its label, is the
/// authoritative identity. Reservations have no token ID to compare.
fn matches_pending_token_id(status: Option<&KanidmRadiusServerStatus>, token_id: &str) -> bool {
    status
        .and_then(|status| status.pending_token_id.as_deref())
        .is_none_or(|recorded_id| recorded_id == token_id)
}

/// Revoke a stranded, write-only bearer after independently observing that no
/// owned workload can consume this generation. A Secret must be absent during
/// normal recovery; deletion permits an owned immutable Secret after drain.
///
/// A caller cannot supply a boolean "drained" assertion: the guard reads the
/// authoritative Kubernetes API itself and fails closed on every API error.
pub async fn revoke_drained_orphan_token(
    client: &KanidmClient,
    kube_client: Client,
    radius: &KanidmRadiusServer,
    generation: &str,
) -> Result<bool, RadiusIdentityReconcileError> {
    if !pending_orphan(radius.status.as_ref(), generation) {
        return Err(RadiusIdentityReconcileError::OwnershipConflict);
    }
    let identity = identity_for(radius)?;
    let namespace = radius
        .namespace()
        .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
    let uid = radius
        .metadata
        .uid
        .as_deref()
        .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
    let secret_name = config_secret_name(uid, generation)
        .map_err(|_| RadiusIdentityReconcileError::OwnershipConflict)?;
    let secret_api: Api<Secret> = Api::namespaced(kube_client.clone(), &namespace);
    match secret_api.get(&secret_name).await {
        Ok(secret) => {
            // During ordinary reconciliation, a persisted Secret must be
            // recovered, never revoked. During deletion, the finalizer has
            // already drained consumers; accept only the exact UID-owned
            // immutable generation as evidence for cleanup.
            if radius.metadata.deletion_timestamp.is_none() {
                return Ok(false);
            }
            let owner = radius
                .controller_owner_ref(&())
                .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
            verify_owner(&secret.metadata, &owner)
                .map_err(|_| RadiusIdentityReconcileError::OwnershipConflict)?;
            if secret.immutable != Some(true) {
                return Err(RadiusIdentityReconcileError::OwnershipConflict);
            }
        }
        Err(kube::Error::Api(error)) if error.code == 404 => {}
        Err(_) => return Err(RadiusIdentityReconcileError::KubernetesUnavailable),
    }

    // The Deployment may create a new pod after an earlier pod LIST. Refuse
    // cleanup when its current template still references the orphan revision.
    let deployments: Api<Deployment> = Api::namespaced(kube_client.clone(), &namespace);
    match deployments.get(&radius.name_any()).await {
        Ok(deployment) => {
            let owner = radius
                .controller_owner_ref(&())
                .ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
            verify_owner(&deployment.metadata, &owner)
                .map_err(|_| RadiusIdentityReconcileError::OwnershipConflict)?;
            let template_revision = deployment
                .spec
                .as_ref()
                .and_then(|spec| spec.template.metadata.as_ref())
                .and_then(|metadata| metadata.annotations.as_ref())
                .and_then(|annotations| annotations.get("kaniop.rs/radius-revision"));
            if template_revision.is_none_or(|revision| revision == generation) {
                return Ok(false);
            }
        }
        Err(kube::Error::Api(error)) if error.code == 404 => {}
        Err(_) => return Err(RadiusIdentityReconcileError::KubernetesUnavailable),
    }

    let pods: Api<Pod> = Api::namespaced(kube_client, &namespace);
    let selector = format!("kaniop.rs/radius-uid={uid}");
    let observed = pods
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|_| RadiusIdentityReconcileError::KubernetesUnavailable)?;
    if observed.items.iter().any(|pod| {
        pod.metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("kaniop.rs/radius-revision"))
            .is_none_or(|revision| revision == generation)
    }) {
        return Ok(false);
    }

    let label = candidate_token_label(generation)
        .map_err(|_| RadiusIdentityReconcileError::OwnershipConflict)?;
    let entry = client
        .idm_service_account_get(&identity.account_name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    let Some(entry) = entry else {
        return Ok(false);
    };
    if !owned_entry(&entry, &identity) {
        return Err(RadiusIdentityReconcileError::OwnershipConflict);
    }
    let tokens = client
        .idm_service_account_list_api_token(&identity.account_name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    let matching: Vec<_> = tokens.iter().filter(|token| token.label == label).collect();
    if matching.len() > 1 {
        return Err(RadiusIdentityReconcileError::OwnershipConflict);
    }
    if let Some(token) = matching.first() {
        // A committed pending token has an authoritative ID in status.
        // A matching label alone must never authorize its destruction.
        if !matches_pending_token_id(
            radius.status.as_ref(),
            &token.token_id.to_string(),
        ) {
            return Err(RadiusIdentityReconcileError::OwnershipConflict);
        }
        client
            .idm_service_account_destroy_api_token(&identity.account_name, token.token_id)
            .await
            .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    }
    // A token removed by another reconciliation or an administrator is
    // already absent. After proving no consumers remain, that is a completed
    // cleanup, not a reason to strand the pending checkpoint forever.
    Ok(true)
}

/// Ensure a per-CR account and only its own membership edge. This does not
/// mint, rotate or revoke API tokens: token lifecycle is a separate state
/// machine, and cannot reuse mail sender's destructive rotation.
pub async fn ensure_radius_identity(
    client: &KanidmClient,
    radius: &KanidmRadiusServer,
) -> Result<RadiusIdentity, RadiusIdentityReconcileError> {
    let identity = identity_for(radius)?;
    let name = &identity.account_name;

    match client
        .idm_service_account_get(name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?
    {
        Some(entry) if owned_entry(&entry, &identity) => {}
        Some(_) => return Err(RadiusIdentityReconcileError::OwnershipConflict),
        None => {
            match client
                .idm_service_account_create(name, &identity.display_name, ENTRY_MANAGER)
                .await
            {
                Ok(()) => {}
                Err(error) if conflict(&error) => {
                    // A concurrent reconciler may have created the account.
                    // Verify the ownership marker rather than adopting it.
                    let entry = client
                        .idm_service_account_get(name)
                        .await
                        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
                    if !entry
                        .as_ref()
                        .is_some_and(|entry| owned_entry(entry, &identity))
                    {
                        return Err(RadiusIdentityReconcileError::OwnershipConflict);
                    }
                }
                Err(_) => return Err(RadiusIdentityReconcileError::KanidmUnavailable),
            }
        }
    }

    let members = client
        .idm_group_get_members(RADIUS_GROUP)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?
        .unwrap_or_default();
    if !members
        .iter()
        .any(|member| normalize_spn(member) == normalize_spn(name))
    {
        client
            .idm_group_add_members(RADIUS_GROUP, &[name.as_str()])
            .await
            .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    }

    Ok(identity)
}

/// Finalize ONLY after the owned Kubernetes Deployment has been removed and
/// its pods fully terminated. A caller must never treat Deployment deletion
/// acceptance as proof that pods have drained.
pub async fn cleanup_radius_identity_after_drain(
    client: &KanidmClient,
    radius: &KanidmRadiusServer,
) -> Result<(), RadiusIdentityReconcileError> {
    let identity = identity_for(radius)?;
    let name = &identity.account_name;
    let entry = client
        .idm_service_account_get(name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    let Some(entry) = entry else {
        // Already deleted; do not touch any other service account or group.
        return Ok(());
    };
    if !owned_entry(&entry, &identity) {
        return Err(RadiusIdentityReconcileError::OwnershipConflict);
    }

    let tokens = client
        .idm_service_account_list_api_token(name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    // Unknown or unpersisted tokens block finalization rather than being
    // deleted based solely on a plausible-looking generated label.
    if tokens.iter().any(|token| {
        !is_recorded_radius_token(
            radius.status.as_ref(),
            &token.token_id.to_string(),
            &token.label,
        )
    }) {
        return Err(RadiusIdentityReconcileError::ForeignTokensPresent);
    }
    for token in tokens {
        client
            .idm_service_account_destroy_api_token(name, token.token_id)
            .await
            .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    }

    let members = client
        .idm_group_get_members(RADIUS_GROUP)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?
        .unwrap_or_default();
    if members
        .iter()
        .any(|member| normalize_spn(member) == normalize_spn(name))
    {
        // Remove exactly this UID-scoped membership edge, never mutate the
        // shared group's other members or its metadata.
        client
            .idm_group_remove_members(RADIUS_GROUP, &[name.as_str()])
            .await
            .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    }

    client
        .idm_service_account_delete(name)
        .await
        .map_err(|_| RadiusIdentityReconcileError::KanidmUnavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_cr_uid_marker_authorizes_existing_account() {
        let a = RadiusIdentity::new("network", "campus", "d10066c3-4599-4487-972d-26bb42587a32")
            .unwrap();
        let mut entry: Entry = serde_json::from_value(serde_json::json!({
            "attrs": {"displayname": [a.display_name.clone()]}
        }))
        .unwrap();
        assert!(owned_entry(&entry, &a));
        entry
            .attrs
            .insert("displayname".into(), vec!["other resource".into()]);
        assert!(!owned_entry(&entry, &a));
        entry.attrs.remove("displayname");
        assert!(!owned_entry(&entry, &a));
    }

    #[test]
    fn finalization_requires_exact_recorded_token_identity() {
        let generation = "f1085999-9e32-4a9e-ad8e-3cae381b30c6";
        let id = "9ad74e68-147f-49e8-96db-dc21c920ff3b";
        let label = candidate_token_label(generation).unwrap();
        let status = KanidmRadiusServerStatus {
            active_token_id: Some(id.into()),
            active_token_generation: Some(generation.into()),
            ..Default::default()
        };
        assert!(is_recorded_radius_token(Some(&status), id, &label));
        assert!(!is_recorded_radius_token(None, id, &label));
        assert!(!is_recorded_radius_token(
            Some(&status),
            "foreign-token",
            &label
        ));
        assert!(!is_recorded_radius_token(
            Some(&status),
            id,
            "kaniop-radius-31b30b65037a418fb7ca0c376da2db28"
        ));
        let reserved = KanidmRadiusServerStatus {
            pending_token_generation: Some(generation.into()),
            ..Default::default()
        };
        assert!(!is_recorded_radius_token(Some(&reserved), id, &label));
    }

    #[test]
    fn orphan_cleanup_requires_exact_unissued_pending_generation() {
        let generation = "f1085999-9e32-4a9e-ad8e-3cae381b30c6";
        let mut status = KanidmRadiusServerStatus {
            pending_token_generation: Some(generation.into()),
            ..Default::default()
        };
        assert!(reserved_orphan(Some(&status), generation));
        assert!(!reserved_orphan(None, generation));
        assert!(!reserved_orphan(Some(&status), "other"));
        status.active_token_generation = Some(generation.into());
        assert!(!reserved_orphan(Some(&status), generation));
        status.active_token_generation = None;
        status.pending_token_id = Some("issued".into());
        assert!(!reserved_orphan(Some(&status), generation));
        status.pending_token_id = None;
        status.pending_config_secret = Some("already-persisted".into());
        assert!(!reserved_orphan(Some(&status), generation));
    }

    #[test]
    fn pending_orphan_guard_rejects_active_and_foreign_generations() {
        let generation = "f1085999-9e32-4a9e-ad8e-3cae381b30c6";
        let reserved = KanidmRadiusServerStatus {
            pending_token_generation: Some(generation.into()),
            ..Default::default()
        };
        assert!(pending_orphan(Some(&reserved), generation));
        assert!(!pending_orphan(Some(&reserved), "other-generation"));

        let issued = KanidmRadiusServerStatus {
            pending_token_generation: Some(generation.into()),
            pending_token_id: Some("9ad74e68-147f-49e8-96db-dc21c920ff3b".into()),
            pending_config_secret: Some("owned-generation".into()),
            ..Default::default()
        };
        assert!(pending_orphan(Some(&issued), generation));
        assert!(matches_pending_token_id(
            Some(&issued),
            "9ad74e68-147f-49e8-96db-dc21c920ff3b"
        ));
        assert!(!matches_pending_token_id(Some(&issued), "foreign-token-id"));
        assert!(matches_pending_token_id(Some(&reserved), "unrecorded-token-id"));
        let mut active = issued.clone();
        active.active_token_generation = Some(generation.into());
        assert!(!pending_orphan(Some(&active), generation));
        let mut partial = issued;
        partial.pending_config_secret = None;
        assert!(!pending_orphan(Some(&partial), generation));
        assert!(!pending_orphan(None, generation));
    }

    #[test]
    fn normalized_group_membership_is_idempotent() {
        let name = "kaniop_radius_d10066c345994487972d26bb42587a32";
        let members = ["kaniop_radius_d10066c345994487972d26bb42587a32@idm.example.com"];
        assert!(
            members
                .iter()
                .any(|entry| normalize_spn(entry) == normalize_spn(name))
        );
    }
}
