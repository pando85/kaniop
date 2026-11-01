use json_patch::merge;
use k8s_openapi::api::core::v1::{Container, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub fn merge_containers(
    containers: Option<Vec<Container>>,
    container: &Container,
) -> Result<Vec<Container>> {
    let merged_containers: Vec<Container> = containers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            if c.name == container.name {
                let mut base = serde_json::to_value(container).map_err(|e| {
                    Error::SerializationError("serialize container spec".to_string(), e)
                })?;
                let override_value = serde_json::to_value(&c).map_err(|e| {
                    Error::SerializationError("serialize user container".to_string(), e)
                })?;
                merge(&mut base, &override_value);
                serde_json::from_value(base).map_err(|e| {
                    Error::SerializationError("deserialize merged container".to_string(), e)
                })
            } else {
                Ok(c)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(merged_containers
        .clone()
        .into_iter()
        .chain(
            if merged_containers.iter().any(|c| c.name == container.name) {
                None
            } else {
                Some(container.clone())
            },
        )
        .collect())
}

/// Return the image's tag without its digest, if one is explicitly present.
///
/// A registry may contain a port, and a digest contains a colon too. Neither
/// belongs to the image tag used by Kanidm's version/upgrade checks.
#[inline]
pub fn get_image_tag(image: &str) -> Option<String> {
    let repository_and_tag = image.split('@').next()?;
    let image_name = repository_and_tag.rsplit('/').next()?;
    image_name
        .rsplit_once(':')
        .and_then(|(_, tag)| (!tag.is_empty()).then(|| tag.to_string()))
}

/// Compute a deterministic SHA-256 hex digest of a secret's data.
///
/// Entries are hashed in key order (`BTreeMap` iteration) with a NUL separator
/// between keys and values so shifting bytes between them changes the digest.
pub fn hash_secret_data(secret: &Secret) -> String {
    let mut hasher = Sha256::new();
    for (key, value) in secret.data.iter().flatten() {
        hasher.update(key.as_bytes());
        hasher.update([0u8]);
        hasher.update(&value.0);
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

/// Preserve lastTransitionTime when condition type, status, and reason are unchanged.
///
/// Returns the existing transition time if a condition with matching type, status, and reason
/// is found in current_conditions. Otherwise returns the current time. This prevents unnecessary
/// status updates and reconcile storms by maintaining stable transition timestamps.
pub fn last_transition_time(
    current_conditions: Option<&Vec<Condition>>,
    type_: &str,
    new_status: &str,
    new_reason: &str,
) -> Time {
    let now_time = Time(Timestamp::now());
    if let Some(conditions) = current_conditions {
        for c in conditions {
            if c.type_ == type_ && c.status == new_status && c.reason == new_reason {
                return c.last_transition_time.clone();
            }
        }
    }
    now_time
}

#[cfg(test)]
mod test {
    use super::{Container, Secret, get_image_tag, hash_secret_data, merge_containers};

    use std::collections::BTreeMap;

    use k8s_openapi::ByteString;

    const CONTAINER_NAME: &str = "kanidm";

    fn secret_with_data(entries: &[(&str, &[u8])]) -> Secret {
        Secret {
            data: Some(
                entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), ByteString(v.to_vec())))
                    .collect::<BTreeMap<_, _>>(),
            ),
            ..Secret::default()
        }
    }

    #[test]
    fn get_image_tag_uses_tag_not_server_digest_or_registry_port() {
        for (reference, expected) in [
            ("kanidm/server:1.11.2", Some("1.11.2")),
            ("kanidm/server:1.11.2@sha256:deadbeef", Some("1.11.2")),
            ("kanidm/server:latest@sha256:deadbeef", Some("latest")),
            (
                "registry.example.com:5000/kanidm/server:1.11.2@sha256:deadbeef",
                Some("1.11.2"),
            ),
            ("registry.example.com:5000/kanidm/server", None),
            ("kanidm/server@sha256:deadbeef", None),
            ("kanidm/server", None),
            ("kanidm/server:", None),
        ] {
            assert_eq!(get_image_tag(reference).as_deref(), expected, "{reference}");
        }
    }

    #[test]
    fn test_hash_secret_data_is_deterministic() {
        let secret = secret_with_data(&[("tls.crt", b"cert"), ("tls.key", b"key")]);
        assert_eq!(hash_secret_data(&secret), hash_secret_data(&secret));
    }

    #[test]
    fn test_hash_secret_data_changes_with_content() {
        let secret = secret_with_data(&[("tls.crt", b"cert"), ("tls.key", b"key")]);
        let renewed = secret_with_data(&[("tls.crt", b"renewed"), ("tls.key", b"key")]);
        assert_ne!(hash_secret_data(&secret), hash_secret_data(&renewed));
    }

    #[test]
    fn test_hash_secret_data_separates_keys_and_values() {
        let secret = secret_with_data(&[("ab", b"c")]);
        let shifted = secret_with_data(&[("a", b"bc")]);
        assert_ne!(hash_secret_data(&secret), hash_secret_data(&shifted));
    }

    #[test]
    fn test_hash_secret_data_empty() {
        let no_data = Secret::default();
        let empty_data = secret_with_data(&[]);
        assert_eq!(hash_secret_data(&no_data), hash_secret_data(&empty_data));
    }

    #[test]
    fn test_generate_containers_with_existing_kanidm() {
        let containers = Some(vec![Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("overridden:user".to_string()),
            working_dir: Some("/data".to_string()),
            ..Container::default()
        }]);

        let container = Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("overridden:spec".to_string()),
            restart_policy: Some("Always".to_string()),
            ..Container::default()
        };

        let containers = merge_containers(containers, &container).unwrap();
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].name, CONTAINER_NAME);
        assert_eq!(containers[0].image, Some("overridden:user".to_string()));
        assert_eq!(containers[0].restart_policy, Some("Always".to_string()));
        assert_eq!(containers[0].working_dir, Some("/data".to_string()));
        assert!(containers[0].ports.clone().is_none());
    }

    #[test]
    fn kanidm_server_digest_and_container_override_are_preserved() {
        let server = Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("kanidm/server:1.11.2@sha256:serverdigest".to_string()),
            ..Container::default()
        };

        // No override: the Kanidm StatefulSet uses the exact requested server image.
        let inherited = merge_containers(None, &server).unwrap();
        assert_eq!(
            inherited[0].image.as_deref(),
            Some("kanidm/server:1.11.2@sha256:serverdigest")
        );

        // Explicit container override: it replaces the whole image reference,
        // including the repository and digest, not just its tag.
        let override_container = Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("mirror.example.com/kanidm/server:custom@sha256:otherdigest".to_string()),
            ..Container::default()
        };
        let overridden = merge_containers(Some(vec![override_container]), &server).unwrap();
        assert_eq!(
            overridden[0].image.as_deref(),
            Some("mirror.example.com/kanidm/server:custom@sha256:otherdigest")
        );
    }

    #[test]
    fn test_generate_containers_without_existing_kanidm() {
        let containers = Some(vec![Container {
            name: "other".to_string(),
            ..Container::default()
        }]);

        let container = Container {
            name: CONTAINER_NAME.to_string(),
            ..Container::default()
        };

        let containers = merge_containers(containers, &container).unwrap();
        assert_eq!(containers.len(), 2);
        assert!(containers.iter().any(|c| c.name == CONTAINER_NAME));
    }

    #[test]
    fn test_merge_containers_preserves_image_when_user_omits_it() {
        use k8s_openapi::api::core::v1::SecurityContext;

        // User specifies a container with securityContext but no image
        let containers = Some(vec![Container {
            name: CONTAINER_NAME.to_string(),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }]);

        // Operator generates a container with an image
        let container = Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("ghcr.io/rash-sh/rash:2.18.3".to_string()),
            env: Some(vec![]),
            ..Default::default()
        };

        let merged = merge_containers(containers, &container).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, CONTAINER_NAME);
        // Image should be preserved from operator's container
        assert_eq!(
            merged[0].image,
            Some("ghcr.io/rash-sh/rash:2.18.3".to_string())
        );
        // SecurityContext should be from user's container
        assert!(merged[0].security_context.is_some());
        assert_eq!(
            merged[0]
                .security_context
                .as_ref()
                .unwrap()
                .allow_privilege_escalation,
            Some(false)
        );
    }
}
