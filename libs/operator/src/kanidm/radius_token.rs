//! Read-only API token issuance for the owned Kanidm RADIUS service account.
//!
//! This module NEVER destroys an existing token. The controller must first
//! persist a candidate transition and ensure ownership of the UID-bound
//! identity, then store the bearer in an immutable Secret. The token ID may
//! be recorded in status, but neither bearer nor NAS secrets may be logged.

use kanidm_client::KanidmClient;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusTokenError {
    #[error("RADIUS token generation identifier is invalid")]
    InvalidGeneration,
    #[error("RADIUS candidate token label already exists")]
    LabelConflict,
    #[error("Kanidm API token operation failed")]
    KanidmUnavailable,
    #[error("cannot uniquely identify newly issued RADIUS token")]
    AmbiguousIssuedToken,
}

/// A minted bearer MUST be stored in a UID-owned immutable Kubernetes Secret
/// before any candidate rollout. Do not derive Debug or Serialize.
pub struct IssuedRadiusToken {
    pub token_id: Uuid,
    pub label: String,
    pub bearer: String,
}

/// Strict deterministic labels allow restart recovery without ever mixing
/// unrelated consumers' tokens. Every candidate generation is a unique UUID
/// even if its CR name and namespace are later reused.
pub fn candidate_token_label(generation: &str) -> Result<String, RadiusTokenError> {
    let generation = Uuid::parse_str(generation).map_err(|_| RadiusTokenError::InvalidGeneration)?;
    Ok(format!("kaniop-radius-{}", generation.simple()))
}

/// Only tokens matching the exact generated token-label grammar are owned
/// by this subsystem. Arbitrary prefix matches are not sufficient for cleanup.
pub fn is_owned_radius_token_label(label: &str) -> bool {
    let Some(id) = label.strip_prefix("kaniop-radius-") else {
        return false;
    };
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Issue a read-only token for a *previously verified owned* account.
///
/// Call only after storing the generation in status, so a controller restart
/// can recognize its own orphaned token. No destructive retry is attempted
/// if the API returns a timeout or the response cannot be correlated.
pub async fn issue_radius_token(
    client: &KanidmClient,
    owned_account_name: &str,
    generation: &str,
) -> Result<IssuedRadiusToken, RadiusTokenError> {
    let label = candidate_token_label(generation)?;
    let existing = client
        .idm_service_account_list_api_token(owned_account_name)
        .await
        .map_err(|_| RadiusTokenError::KanidmUnavailable)?;
    if existing.iter().any(|token| token.label == label) {
        // The bearer is write-only. Reissuing or adopting the same token label
        // cannot recover its secret; require the controller recovery protocol.
        return Err(RadiusTokenError::LabelConflict);
    }

    let bearer = client
        .idm_service_account_generate_api_token(
            owned_account_name,
            &label,
            None,
            false, // read-only is mandatory for RADIUS
            false, // no RADIUS privilege grants
        )
        .await
        .map_err(|_| RadiusTokenError::KanidmUnavailable)?;

    let found = client
        .idm_service_account_list_api_token(owned_account_name)
        .await
        .map_err(|_| RadiusTokenError::KanidmUnavailable)?;
    let matching: Vec<_> = found.iter().filter(|token| token.label == label).collect();
    if matching.len() != 1
        || existing.iter().any(|old| old.token_id == matching[0].token_id)
    {
        // Preserve every token after an ambiguous result. Reconciliation must
        // resolve it by UID-bound account and generation, not revoke blindly.
        return Err(RadiusTokenError::AmbiguousIssuedToken);
    }
    Ok(IssuedRadiusToken {
        token_id: matching[0].token_id,
        label,
        bearer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_labels_are_unique_and_bounded() {
        let a = candidate_token_label("d10066c3-4599-4487-972d-26bb42587a32").unwrap();
        let b = candidate_token_label("ad8cf3d5-b2a9-44d6-b257-058554ab09d5").unwrap();
        assert_eq!(a, "kaniop-radius-d10066c345994487972d26bb42587a32");
        assert_ne!(a, b);
        assert!(a.len() <= 63);
        assert!(is_owned_radius_token_label(&a));
        assert!(!is_owned_radius_token_label("kaniop-radius-not-a-uuid"));
        assert!(!is_owned_radius_token_label("kaniop-radius-deadbeef"));
        assert!(!is_owned_radius_token_label("other-d10066c345994487972d26bb42587a32"));
        assert_eq!(candidate_token_label("not-uuid"), Err(RadiusTokenError::InvalidGeneration));
    }
}
