//! Deterministic Kanidm identity names for individual RADIUS custom resources.
//!
//! A Kubernetes object name is reusable after deletion. Kanidm accounts live
//! outside Kubernetes garbage collection, so using only namespace/name would
//! accidentally adopt a previous or unrelated account after recreation.
//! Instead the immutable Kubernetes UID identifies the account. Always inspect
//! existing account ownership metadata before updating it; a matching computed
//! name alone must never authorize adoption.

use thiserror::Error;
use uuid::Uuid;

const RADIUS_ACCOUNT_PREFIX: &str = "kaniop_radius_";
const DISPLAY_NAME_PREFIX: &str = "Kaniop RADIUS: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusIdentity {
    /// Stable across controller restarts, distinct across CR re-creations.
    pub account_name: String,
    /// A human-readable ownership marker for the Kanidm display name.
    pub display_name: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusIdentityError {
    #[error("RADIUS resource requires a valid Kubernetes UID")]
    InvalidUid,
    #[error("RADIUS resource requires a namespace and a name")]
    MissingObjectIdentity,
}

impl RadiusIdentity {
    pub fn new(namespace: &str, name: &str, uid: &str) -> Result<Self, RadiusIdentityError> {
        if namespace.is_empty() || name.is_empty() {
            return Err(RadiusIdentityError::MissingObjectIdentity);
        }
        let uid = Uuid::parse_str(uid).map_err(|_| RadiusIdentityError::InvalidUid)?;
        // Kanidm account names permit ASCII underscores. The UID's 128 bits
        // avoid name collision between independent RADIUS resources without
        // requiring a shared lock or incrementing counter.
        let account_name = format!("{RADIUS_ACCOUNT_PREFIX}{}", uid.simple());
        let display_name = format!(
            "{DISPLAY_NAME_PREFIX}{namespace}/{name} [{}]",
            uid.hyphenated()
        );
        Ok(Self {
            account_name,
            display_name,
        })
    }

    /// Do not silently adopt an account if an operator restart finds an
    /// already-existing Kanidm entry. The caller must inspect the account's
    /// display name and return an ownership conflict when this is false.
    pub fn matches_existing_display_name(&self, display_name: &str) -> bool {
        self.display_name == display_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_UID: &str = "d10066c3-4599-4487-972d-26bb42587a32";
    const SECOND_UID: &str = "ad8cf3d5-b2a9-44d6-b257-058554ab09d5";

    #[test]
    fn identity_is_stable_across_reconciliation() {
        let a = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        let b = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.account_name,
            "kaniop_radius_d10066c345994487972d26bb42587a32"
        );
        assert!(a.matches_existing_display_name(&a.display_name));
    }

    #[test]
    fn same_name_recreated_with_new_uid_does_not_adopt_previous_account() {
        let old = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        let new = RadiusIdentity::new("network", "radius", SECOND_UID).unwrap();
        assert_ne!(old.account_name, new.account_name);
        assert!(!new.matches_existing_display_name(&old.display_name));
    }

    #[test]
    fn different_resources_cannot_share_the_same_uid_identity_marker() {
        // An unexpectedly reused UID would not be sufficient to authorize an
        // existing account with different namespace/name metadata.
        let a = RadiusIdentity::new("network-a", "radius", FIRST_UID).unwrap();
        let b = RadiusIdentity::new("network-b", "radius", FIRST_UID).unwrap();
        assert_eq!(a.account_name, b.account_name);
        assert!(!b.matches_existing_display_name(&a.display_name));
    }

    #[test]
    fn rejects_absent_or_non_uuid_uid() {
        assert_eq!(
            RadiusIdentity::new("network", "radius", ""),
            Err(RadiusIdentityError::InvalidUid)
        );
        assert_eq!(
            RadiusIdentity::new("network", "radius", "wrong-uid"),
            Err(RadiusIdentityError::InvalidUid)
        );
        assert_eq!(
            RadiusIdentity::new("", "radius", FIRST_UID),
            Err(RadiusIdentityError::MissingObjectIdentity)
        );
    }

    #[test]
    fn forged_display_name_causes_ownership_conflict() {
        let identity = RadiusIdentity::new("network", "radius", FIRST_UID).unwrap();
        assert!(!identity.matches_existing_display_name("Someone else's service account"));
    }
}
