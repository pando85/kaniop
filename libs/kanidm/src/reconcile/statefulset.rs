use crate::crd::Kanidm;

use std::collections::BTreeMap;

use json_patch::merge;
use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EmptyDirVolumeSource, EnvVar, HTTPGetAction, PersistentVolumeClaim,
    PodSpec, PodTemplateSpec, Probe, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

const CONTAINER_NAME: &str = "kanidm";
const CONTAINER_HTTPS_PORT: i32 = 8443;
const CONTAINER_LDAP_PORT: i32 = 3636;
const VOLUME_DATA_NAME: &str = "kanidm-data";

pub trait StatefulSetExt {
    fn generate_containers(&self, kanidm_container: &Container) -> Vec<Container>;
    fn generate_storage(&self) -> (Option<Vec<Volume>>, Option<Vec<PersistentVolumeClaim>>);
    // TODO: clean
    #[allow(dead_code)]
    fn get_statefulset(&self, replica: &i32) -> StatefulSet;
}

impl StatefulSetExt for Kanidm {
    fn generate_containers(&self, kanidm_container: &Container) -> Vec<Container> {
        let merged_containers: Vec<Container> = self
            .spec
            .containers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|mut container| {
                if container.name == CONTAINER_NAME {
                    merge(
                        // safe unwrap: we know the container is serializable
                        &mut serde_json::to_value(&mut container).unwrap(),
                        &serde_json::to_value(kanidm_container).unwrap(),
                    );
                }
                container
            })
            .collect();

        if merged_containers.iter().any(|c| c.name == CONTAINER_NAME) {
            merged_containers
        } else {
            merged_containers
                .into_iter()
                .chain(std::iter::once(kanidm_container.clone()))
                .collect()
        }
    }

    fn generate_storage(&self) -> (Option<Vec<Volume>>, Option<Vec<PersistentVolumeClaim>>) {
        match self.spec.storage.clone() {
            Some(storage) => {
                if let Some(empty_dir) = storage.empty_dir {
                    let volumes = self
                        .spec
                        .volumes
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .chain(std::iter::once(Volume {
                            name: VOLUME_DATA_NAME.to_string(),
                            empty_dir: Some(empty_dir),
                            ..Volume::default()
                        }))
                        .collect();
                    (Some(volumes), None)
                } else if let Some(ephemeral) = storage.ephemeral {
                    let volumes = self
                        .spec
                        .volumes
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .chain(std::iter::once(Volume {
                            name: VOLUME_DATA_NAME.to_string(),
                            ephemeral: Some(ephemeral),
                            ..Volume::default()
                        }))
                        .collect();
                    (Some(volumes), None)
                } else if let Some(volume_claim_template) = storage.volume_claim_template {
                    (self.spec.volumes.clone(), Some(vec![volume_claim_template]))
                } else {
                    let volumes = self
                        .spec
                        .volumes
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .chain(std::iter::once(Volume {
                            name: VOLUME_DATA_NAME.to_string(),
                            empty_dir: Some(EmptyDirVolumeSource {
                                medium: None,
                                size_limit: None,
                            }),
                            ..Volume::default()
                        }))
                        .collect();
                    (Some(volumes), None)
                }
            }
            None => {
                let volumes = self
                    .spec
                    .volumes
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .chain(std::iter::once(Volume {
                        name: VOLUME_DATA_NAME.to_string(),
                        empty_dir: Some(EmptyDirVolumeSource {
                            medium: None,
                            size_limit: None,
                        }),
                        ..Volume::default()
                    }))
                    .collect();
                (Some(volumes), None)
            }
        }
    }

    fn get_statefulset(&self, replica: &i32) -> StatefulSet {
        let name = self.name_any();
        let pod_labels: BTreeMap<String, String> = self
            .get_labels()
            .into_iter()
            .chain([
                ("kanidm.kaniop.rs/cluster".to_string(), name.clone()),
                ("kanidm.kaniop.rs/replica".to_string(), replica.to_string()),
            ])
            .collect();

        let labels: BTreeMap<String, String> = self
            .labels()
            .clone()
            .into_iter()
            .chain(pod_labels.clone())
            .collect();

        let ports = std::iter::once(ContainerPort {
            name: Some(self.spec.port_name.clone()),
            container_port: 8443,
            ..ContainerPort::default()
        })
        .chain(
            self.spec
                .ldap_port_name
                .clone()
                .into_iter()
                .map(|port_name| ContainerPort {
                    name: Some(port_name.clone()),
                    container_port: 3636,
                    ..ContainerPort::default()
                }),
        )
        .collect();

        let env: Vec<EnvVar> = vec![
            EnvVar {
                name: "KANIDM_DOMAIN".to_string(),
                value: Some(self.spec.domain.clone()),
                ..EnvVar::default()
            },
            EnvVar {
                name: "KANIDM_BINDADDRESS".to_string(),
                value: Some(format!("0.0.0.0:{CONTAINER_HTTPS_PORT}")),
                ..EnvVar::default()
            },
            EnvVar {
                name: "KANIDM_LOG_LEVEL".to_string(),
                value: Some(
                    serde_json::to_string(&self.spec.log_level.clone())
                        // safe unwrap: we know the log level is serializable
                        .unwrap(),
                ),
                ..EnvVar::default()
            },
        ]
        .into_iter()
        .chain(
            self.spec
                .ldap_port_name
                .clone()
                .into_iter()
                .map(|_| EnvVar {
                    name: "KANIDM_LDAPBINDADDRESS".to_string(),
                    value: Some(format!("0.0.0.0:{CONTAINER_LDAP_PORT}")),
                    ..EnvVar::default()
                }),
        )
        .collect();

        let probe = Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/status".to_string()),
                port: IntOrString::String(self.spec.port_name.clone()),
                scheme: Some("HTTPS".to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        };

        let volume_mounts = Some(
            self.spec
                .volume_mounts
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(std::iter::once(VolumeMount {
                    name: VOLUME_DATA_NAME.to_string(),
                    mount_path: "/data".to_string(),
                    ..VolumeMount::default()
                }))
                .collect(),
        );
        let kanidm_container = &Container {
            name: CONTAINER_NAME.to_string(),
            image: Some(self.spec.image.clone()),
            image_pull_policy: self.spec.image_pull_policy.clone(),
            env: Some(env),
            ports: Some(ports),
            volume_mounts,
            resources: self.spec.resources.clone(),
            readiness_probe: Some(probe.clone()),
            liveness_probe: Some(probe),
            ..Container::default()
        };

        let dns_policy = match self.spec.host_network {
            Some(true) => Some("ClusterFirstWithHostNet".to_string()),
            _ => self.spec.dns_policy.clone(),
        };

        let containers = self.generate_containers(kanidm_container);
        let (volumes, volume_claim_templates) = self.generate_storage();
        StatefulSet {
            metadata: ObjectMeta {
                name: Some(name),
                namespace: self.namespace(),
                labels: Some(labels.clone()),
                owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
                annotations: Some(self.annotations().to_owned()),
                ..ObjectMeta::default()
            },
            spec: Some(StatefulSetSpec {
                replicas: Some(1),
                selector: LabelSelector {
                    match_expressions: None,
                    match_labels: Some(pod_labels.clone()),
                },
                template: PodTemplateSpec {
                    // TODO: define pod labels
                    metadata: Some(ObjectMeta {
                        labels: Some(pod_labels),
                        ..ObjectMeta::default()
                    }),
                    spec: Some(PodSpec {
                        containers,
                        volumes,
                        node_selector: self.spec.node_selector.clone(),
                        affinity: self.spec.affinity.clone(),
                        tolerations: self.spec.tolerations.clone(),
                        topology_spread_constraints: self.spec.topology_spread_constraints.clone(),
                        security_context: self.spec.security_context.clone(),
                        dns_policy,
                        dns_config: self.spec.dns_config.clone(),
                        init_containers: self.spec.init_containers.clone(),
                        host_aliases: self.spec.host_aliases.clone(),
                        ..PodSpec::default()
                    }),
                },
                persistent_volume_claim_retention_policy: self
                    .spec
                    .persistent_volume_claim_retention_policy
                    .clone(),
                min_ready_seconds: self.spec.min_ready_seconds,
                volume_claim_templates,
                ..StatefulSetSpec::default()
            }),
            ..StatefulSet::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::crd::{Kanidm, KanidmSpec, KanidmStorage};
    use k8s_openapi::api::core::v1::Container;
    use k8s_openapi::api::core::v1::{
        EmptyDirVolumeSource, EphemeralVolumeSource, PersistentVolumeClaim, Volume,
    };

    #[test]
    fn test_generate_containers_with_existing_kanidm() {
        let kanidm = Kanidm {
            spec: KanidmSpec {
                containers: Some(vec![Container {
                    name: CONTAINER_NAME.to_string(),
                    image: Some("overridden:latest".to_string()),
                    ..Container::default()
                }]),
                domain: "example.com".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let kanidm_container = Container {
            name: CONTAINER_NAME.to_string(),
            ..Container::default()
        };

        let containers = kanidm.generate_containers(&kanidm_container);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].name, CONTAINER_NAME);
        assert_eq!(containers[0].image, Some("overridden:latest".to_string()));
        assert!(containers[0].ports.clone().is_none());
    }

    #[test]
    fn test_generate_containers_without_existing_kanidm() {
        let kanidm = Kanidm {
            spec: KanidmSpec {
                containers: Some(vec![Container {
                    name: "other".to_string(),
                    ..Container::default()
                }]),
                domain: "example.com".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let kanidm_container = Container {
            name: CONTAINER_NAME.to_string(),
            ..Container::default()
        };

        let containers = kanidm.generate_containers(&kanidm_container);
        assert_eq!(containers.len(), 2);
        assert!(containers.iter().any(|c| c.name == CONTAINER_NAME));
    }

    fn create_kanidm_with_storage_and_volumes(
        storage: Option<KanidmStorage>,
        volumes: Option<Vec<Volume>>,
    ) -> Kanidm {
        Kanidm {
            spec: KanidmSpec {
                storage,
                volumes,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_volumes_without_storage() {
        let kanidm = create_kanidm_with_storage_and_volumes(None, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert_eq!(
            volumes.clone().unwrap().first().unwrap().name,
            "kanidm-data"
        );
        assert!(volumes.unwrap().first().unwrap().empty_dir.is_some());
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_emptydir() {
        let storage = Some(KanidmStorage {
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_emptydir_and_ephemeral() {
        let storage = Some(KanidmStorage {
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ephemeral: Some(EphemeralVolumeSource::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_emptydir_ephemeral_and_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ephemeral: Some(EphemeralVolumeSource::default()),
            volume_claim_template: Some(PersistentVolumeClaim::default()),
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_ephemeral() {
        let storage = Some(KanidmStorage {
            ephemeral: Some(EphemeralVolumeSource::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.ephemeral.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_ephemeral_and_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            ephemeral: Some(EphemeralVolumeSource::default()),
            volume_claim_template: Some(PersistentVolumeClaim::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 1);
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.ephemeral.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            volume_claim_template: Some(PersistentVolumeClaim::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage_and_volumes(storage, None);
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert!(volumes.is_none());
        assert!(volume_claim_template.is_some());
    }

    #[test]
    fn test_generate_volumes_with_existing_volumes() {
        let existing_volume = Volume {
            name: "existing-volume".to_string(),
            ..Volume::default()
        };
        let kanidm =
            create_kanidm_with_storage_and_volumes(None, Some(vec![existing_volume.clone()]));
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 2);
        assert!(volumes
            .clone()
            .unwrap()
            .iter()
            .any(|v| v.name == "existing-volume"));
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_multiple_existing_volumes() {
        let existing_volume1 = Volume {
            name: "existing-volume-1".to_string(),
            ..Volume::default()
        };
        let existing_volume2 = Volume {
            name: "existing-volume-2".to_string(),
            ..Volume::default()
        };
        let kanidm = create_kanidm_with_storage_and_volumes(
            None,
            Some(vec![existing_volume1.clone(), existing_volume2.clone()]),
        );
        let (volumes, volume_claim_template) = kanidm.generate_storage();

        assert_eq!(volumes.clone().unwrap().len(), 3);
        assert!(volumes
            .clone()
            .unwrap()
            .iter()
            .any(|v| v.name == "existing-volume-1"));
        assert!(volumes
            .clone()
            .unwrap()
            .iter()
            .any(|v| v.name == "existing-volume-2"));
        assert!(volumes
            .unwrap()
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }
}
