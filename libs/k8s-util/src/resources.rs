use json_patch::merge;
use k8s_openapi::api::core::v1::{Container, Secret};
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

#[inline]
pub fn get_image_tag(image: &str) -> Option<String> {
    image.split_once(':').map(|(_, tag)| tag.to_string())
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

#[cfg(test)]
mod test {
    use super::{Container, Secret, hash_secret_data, merge_containers};

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
}
