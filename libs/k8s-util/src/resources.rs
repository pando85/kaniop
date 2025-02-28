use json_patch::merge;
use k8s_openapi::api::core::v1::Container;

pub fn merge_containers(
    containers: Option<Vec<Container>>,
    container: &Container,
) -> Vec<Container> {
    let merged_containers: Vec<Container> = containers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|mut c| {
            if c.name == container.name {
                merge(
                    // safe unwrap: we know the container is serializable
                    &mut serde_json::to_value(&mut c).unwrap(),
                    &serde_json::to_value(container).unwrap(),
                );
            }
            c
        })
        .collect();

    merged_containers
        .clone()
        .into_iter()
        .chain(
            if merged_containers.iter().any(|c| c.name == container.name) {
                None
            } else {
                Some(container.clone())
            },
        )
        .collect()
}

#[cfg(test)]
mod test {
    use super::{Container, merge_containers};

    const CONTAINER_NAME: &str = "kanidm";

    #[test]
    fn test_generate_containers_with_existing_kanidm() {
        let containers = Some(vec![Container {
            name: CONTAINER_NAME.to_string(),
            image: Some("overridden:latest".to_string()),
            ..Container::default()
        }]);

        let container = Container {
            name: CONTAINER_NAME.to_string(),
            ..Container::default()
        };

        let containers = merge_containers(containers, &container);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].name, CONTAINER_NAME);
        assert_eq!(containers[0].image, Some("overridden:latest".to_string()));
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

        let containers = merge_containers(containers, &container);
        assert_eq!(containers.len(), 2);
        assert!(containers.iter().any(|c| c.name == CONTAINER_NAME));
    }
}
