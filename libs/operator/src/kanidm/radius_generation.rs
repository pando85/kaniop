//! Collision-resistant, bounded names for UID-bound RADIUS generations.
//!
//! The Kubernetes resource name can be reused by a different object UID.
//! Generated Secrets and token revisions instead depend on the immutable
//! object UID plus a persistent random generation ID. Owner verification
//! remains mandatory; the shortened UID here is never sole proof of ownership.

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusGenerationError {
    #[error("RADIUS resource UID or generation is not a UUID")]
    InvalidIdentifier,
}

pub fn config_secret_name(uid: &str, generation: &str) -> Result<String, RadiusGenerationError> {
    let uid = Uuid::parse_str(uid).map_err(|_| RadiusGenerationError::InvalidIdentifier)?;
    let generation = Uuid::parse_str(generation)
        .map_err(|_| RadiusGenerationError::InvalidIdentifier)?;
    let uid_hex = uid.simple().to_string();
    let generation_hex = generation.simple().to_string();
    // Both components are lowercase ASCII hex. At most 57 chars so each
    // generation is a valid DNS label; the full UID remains in ownerReferences.
    Ok(format!("krs-{}-{}", &uid_hex[..20], generation_hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_recreated_and_next_generation_do_not_reuse_secret_name() {
        let a = config_secret_name(
            "d10066c3-4599-4487-972d-26bb42587a32",
            "f1085999-9e32-4a9e-ad8e-3cae381b30c6",
        )
        .unwrap();
        let b = config_secret_name(
            "ad8cf3d5-b2a9-44d6-b257-058554ab09d5",
            "f1085999-9e32-4a9e-ad8e-3cae381b30c6",
        )
        .unwrap();
        let c = config_secret_name(
            "d10066c3-4599-4487-972d-26bb42587a32",
            "31b30b65-037a-418f-b7ca-0c376da2db28",
        )
        .unwrap();
        assert_eq!(a, "krs-d10066c345994487972d-f10859999e324a9ead8e3cae381b30c6");
        assert!(a.len() <= 63);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn reject_non_uuid_identifiers() {
        assert_eq!(
            config_secret_name("not-uid", "f1085999-9e32-4a9e-ad8e-3cae381b30c6"),
            Err(RadiusGenerationError::InvalidIdentifier)
        );
    }
}
