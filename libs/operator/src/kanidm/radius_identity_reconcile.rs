//! Dedicated UID-bound Kanidm identity for RADIUS.
//!
//! Each RADIUS resource owns exactly one Kanidm service account, joined to
//! the built-in idm_radius_servers group *additively*. We deliberately never
//! use group_set_members, which could revoke unrelated RADIUS instances.
//! Call only after namespace authorization, restore guard and input preflight.

use kanidm_client::{ClientError, KanidmClient};
use kanidm_proto::v1::Entry;
use kaniop_k8s_util::types::normalize_spn;
use kube::ResourceExt;
use thiserror::Error;

use super::radius_crd::{KanidmRadiusServer, KanidmRadiusServerStatus};
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

fn identity_for(radius: &KanidmRadiusServer) -> Result<RadiusIdentity, RadiusIdentityReconcileError> {
    let namespace = radius.namespace().ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
    let uid = radius.metadata.uid.as_deref().ok_or(RadiusIdentityReconcileError::MissingMetadata)?;
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
                    if !entry.as_ref().is_some_and(|entry| owned_entry(entry, &identity)) {
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
        let a = RadiusIdentity::new(
            "network", "campus", "d10066c3-4599-4487-972d-26bb42587a32",
        )
        .unwrap();
        let mut entry: Entry = serde_json::from_value(serde_json::json!({
            "attrs": {"displayname": [a.display_name.clone()]}
        })).unwrap();
        assert!(owned_entry(&entry, &a));
        entry.attrs.insert("displayname".into(), vec!["other resource".into()]);
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
        assert!(!is_recorded_radius_token(Some(&status), "foreign-token", &label));
        assert!(!is_recorded_radius_token(Some(&status), id, "kaniop-radius-31b30b65037a418fb7ca0c376da2db28"));
        let reserved = KanidmRadiusServerStatus {
            pending_token_generation: Some(generation.into()),
            ..Default::default()
        };
        assert!(!is_recorded_radius_token(Some(&reserved), id, &label));
    }

    #[test]
    fn normalized_group_membership_is_idempotent() {
        let name = "kaniop_radius_d10066c345994487972d26bb42587a32";
        let members = ["kaniop_radius_d10066c345994487972d26bb42587a32@idm.example.com"];
        assert!(members.iter().any(|entry| normalize_spn(entry) == normalize_spn(name)));
    }
}
