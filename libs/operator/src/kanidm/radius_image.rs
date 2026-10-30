//! Conservative image selection for the independently deployed Kanidm RADIUS.
//!
//! A Kanidm server image digest is NOT a RADIUS image digest. The only safe
//! automatic derivation is the same pinned release tag from a Docker Hub
//! kanidm/server image. For nonstandard registries, moving tags and digest-only
//! references, the user must pin an explicit RADIUS image.

use thiserror::Error;

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

    // Only infer Docker Hub Kanidm server image tags. An image digest cannot
    // be converted into a digest for another component.
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
    ) || !valid_release_tag(tag)
    {
        return Err(RadiusImageError::ExplicitImageRequired);
    }

    Ok(format!("kanidm/radius:{tag}"))
}

fn valid_release_tag(tag: &str) -> bool {
    // Require explicit major.minor.patch. Never auto-follow moving tags.
    let core = tag.split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
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
