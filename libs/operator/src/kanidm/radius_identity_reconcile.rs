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

use super::radius_crd::KanidmRadiusServer;
use super::radius_identity::{RadiusIdentity, RadiusIdentityError};

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
    fn normalized_group_membership_is_idempotent() {
        let name = "kaniop_radius_d10066c345994487972d26bb42587a32";
        let members = ["kaniop_radius_d10066c345994487972d26bb42587a32@idm.example.com"];
        assert!(members.iter().any(|entry| normalize_spn(entry) == normalize_spn(name)));
    }
}
