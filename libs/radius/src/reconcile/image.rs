//! Conservative image selection for the independently deployed Kanidm RADIUS.
//!
//! A Kanidm server image digest is NOT a RADIUS image digest. The only safe
//! automatic derivation is the same pinned release tag from a Docker Hub
//! kanidm/server image. For nonstandard registries, moving tags and digest-only
//! references, the user must pin an explicit RADIUS image.

use kaniop_operator::version::is_version_compatible;
use thiserror::Error;

/// Numeric user of the `radiusd` account in the verified image. Kubelet cannot
/// verify `runAsNonRoot` for a named image `USER`, so the pod sets it explicitly.
pub const RADIUS_UID: i64 = 497;
/// Numeric group of the `radiusd` account in the verified image; also the pod
/// `fsGroup` so projected Secret files are group-readable.
pub const RADIUS_GID: i64 = 496;

/// First upstream release validated with the RADIUS runtime contract
/// (see [`RADIUS_UID`] and [`RADIUS_GID`]). The upper compatibility boundary
/// follows the operator's locked Kanidm client version, not a patch allowlist.
const MIN_RADIUS_RELEASE: (u64, u64, u64) = (1, 11, 2);

/// Image inference requires a canonical, stable major.minor.patch tag.
/// The operator's compatibility check alone intentionally accepts unknown
/// tags, so it must only be called after this stricter validation.
fn stable_release(tag: &str) -> Option<(u64, u64, u64)> {
    fn component(value: &str) -> Option<u64> {
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return None;
        }
        value.parse().ok()
    }

    let mut parts = tag.split('.');
    let version = (
        component(parts.next()?)?,
        component(parts.next()?)?,
        component(parts.next()?)?,
    );
    if parts.next().is_some() {
        return None;
    }
    Some(version)
}

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

    // Infer only stable official Docker Hub releases within the shared
    // Kanidm compatibility policy and the minimum RADIUS runtime contract.
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
    ) || !stable_release(tag).is_some_and(|version| version >= MIN_RADIUS_RELEASE)
        || !is_version_compatible(tag)
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
        for tag in ["1.11.2", "1.11.3", "1.12.0", "1.12.1", "1.12.2", "1.12.3"] {
            for repository in [
                "kanidm/server",
                "docker.io/kanidm/server",
                "index.docker.io/kanidm/server",
            ] {
                assert_eq!(
                    resolve_radius_image(&format!("{repository}:{tag}"), None),
                    Ok(format!("kanidm/radius:{tag}"))
                );
            }
        }
    }

    #[test]
    fn does_not_copy_digest_or_guess_nonofficial_registry() {
        for image in [
            "kanidm/server@sha256:deadbeef",
            "docker.io/kanidm/server:latest",
            "ghcr.io/kanidm/server:1.12.1",
            "example.com/my/server:1.12.1",
            "kanidm/server:devel",
            "kanidm/server:1.11.1",
            "kanidm/server:1.10.99",
            "kanidm/server:0.99.99",
            "kanidm/server:1.12",
            "kanidm/server:1.12.3-dev",
            "kanidm/server:1.12.3-rc.1",
            "kanidm/server:1.12.3+build",
            "kanidm/server:v1.12.3",
            "kanidm/server:1.012.3",
            "kanidm/server:1.12.+3",
            "kanidm/server:1.12.3.4",
            "kanidm/server:1.12.3@sha256:deadbeef",
            "kanidm/server:1.12.18446744073709551616",
            "kanidm/server",
        ] {
            assert_eq!(
                resolve_radius_image(image, None),
                Err(RadiusImageError::ExplicitImageRequired)
            );
        }
    }

    #[test]
    fn follows_locked_client_compatibility_without_patch_allowlist() {
        use kaniop_operator::version::KANIDM_CLIENT_VERSION;

        let (major, minor, patch) =
            stable_release(KANIDM_CLIENT_VERSION).expect("stable locked Kanidm version");
        for tag in [
            KANIDM_CLIENT_VERSION.to_string(),
            format!("{major}.{minor}.{}", patch + 1),
        ] {
            assert_eq!(
                resolve_radius_image(&format!("kanidm/server:{tag}"), None),
                Ok(format!("kanidm/radius:{tag}"))
            );
        }
        for tag in [
            format!("{major}.{}.0", minor + 1),
            format!("{}.0.0", major + 1),
        ] {
            assert_eq!(
                resolve_radius_image(&format!("kanidm/server:{tag}"), None),
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
            resolve_radius_image("kanidm/server:1.12.1", Some("  ")),
            Err(RadiusImageError::EmptyImage)
        );
    }
}
