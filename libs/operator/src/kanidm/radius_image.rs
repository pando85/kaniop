//! Conservative image selection for the independently deployed Kanidm RADIUS.
//!
//! A Kanidm server image digest is NOT a RADIUS image digest. The only safe
//! automatic derivation is the same pinned release tag from a Docker Hub
//! kanidm/server image. For nonstandard registries, moving tags and digest-only
//! references, the user must pin an explicit RADIUS image.

use thiserror::Error;

/// Only this upstream release has a validated 497:496 runtime user/group and
/// successful published-image bootstrap smoke test in Kaniop CI.
const VERIFIED_RADIUS_RELEASE: &str = "1.11.2";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusImageError {
    #[error("RADIUS image must not be empty")]
    EmptyImage,
    #[error("explicit RADIUS image is required for an unrecognized or unpinned Kanidm image")]
    ExplicitImageRequired,
}

/// Return the image reference used by the dedicated RADIUS Deployment.
///
/// An override is a complete user-provided image reference and is never
/// rewritten. An explicit digest therefore refers to the RADIUS artifact,
/// rather than inheriting the (incompatible) server image digest.
pub fn resolve_radius_image(
    kanidm_image: &str,
    override_image: Option<&str>,
) -> Result<String, RadiusImageError> {
    if let Some(image) = override_image {
        if image.trim().is_empty() {
            return Err(RadiusImageError::EmptyImage);
        }
        return Ok(image.to_string());
    }

    // Only infer a Docker Hub release validated by published-image CI. Future
    // upstream releases can change the non-root UID/GID or bootstrap contract.
    // Never automatically infer an untested RADIUS image or a server digest.
    let mut parts = kanidm_image.rsplitn(2, ':');
    let tag = parts
        .next()
        .ok_or(RadiusImageError::ExplicitImageRequired)?;
    let name = parts
        .next()
        .ok_or(RadiusImageError::ExplicitImageRequired)?;
    if !matches!(
        name,
        "kanidm/server" | "docker.io/kanidm/server" | "index.docker.io/kanidm/server"
    ) || tag != VERIFIED_RADIUS_RELEASE
    {
        return Err(RadiusImageError::ExplicitImageRequired);
    }

    Ok(format!("kanidm/radius:{tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_official_dockerhub_release_tag_only() {
        for image in [
            "kanidm/server:1.11.2",
            "docker.io/kanidm/server:1.11.2",
            "index.docker.io/kanidm/server:1.11.2",
        ] {
            assert_eq!(
                resolve_radius_image(image, None),
                Ok("kanidm/radius:1.11.2".into())
            );
        }
    }

    #[test]
    fn does_not_copy_digest_or_guess_nonofficial_registry() {
        for image in [
            "kanidm/server@sha256:deadbeef",
            "docker.io/kanidm/server:latest",
            "ghcr.io/kanidm/server:1.11.2",
            "example.com/my/server:1.11.2",
            "kanidm/server:devel",
            "kanidm/server:1.11.3",
            "kanidm/server:1.12.0",
            "kanidm/server:1.11",
            "kanidm/server",
        ] {
            assert_eq!(
                resolve_radius_image(image, None),
                Err(RadiusImageError::ExplicitImageRequired)
            );
        }
    }

    #[test]
    fn overrides_are_passed_through_unmodified() {
        assert_eq!(
            resolve_radius_image(
                "kanidm/server@sha256:deadbeef",
                Some("mirror.example/radius@sha256:abcdef")
            ),
            Ok("mirror.example/radius@sha256:abcdef".into())
        );
    }

    #[test]
    fn explicitly_blank_override_fails_closed() {
        assert_eq!(
            resolve_radius_image("kanidm/server:1.11.2", Some("  ")),
            Err(RadiusImageError::EmptyImage)
        );
    }
}
