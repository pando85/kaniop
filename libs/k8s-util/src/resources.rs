use json_patch::merge;
use k8s_openapi::api::core::v1::Container;

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

#[cfg(test)]
mod test {
    use super::{Container, merge_containers};

    const CONTAINER_NAME: &str = "kanidm";

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
