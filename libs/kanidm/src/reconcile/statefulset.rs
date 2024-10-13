use crate::crd::Kanidm;

use std::collections::BTreeMap;

use json_patch::merge;
use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EmptyDirVolumeSource, EnvVar, HTTPGetAction, PersistentVolumeClaim,
    PodSpec, PodTemplateSpec, Probe, SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

const CONTAINER_NAME: &str = "kanidm";
const CONTAINER_HTTPS_PORT: i32 = 8443;
const CONTAINER_LDAP_PORT: i32 = 3636;
const VOLUME_DATA_NAME: &str = "kanidm-data";
const VOLUME_DATA_PATH: &str = "/data";
const VOLUME_TLS_NAME: &str = "kanidm-certs";
const VOLUME_TLS_PATH: &str = "/etc/kanidm/tls";

pub trait StatefulSetExt {
    fn generate_containers(&self, kanidm_container: &Container) -> Vec<Container>;
    fn expand_storage(
        &self,
        volumes: Vec<Volume>,
    ) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>);
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

    fn expand_storage(
        &self,
        volumes: Vec<Volume>,
    ) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>) {
        let default_expand_storage = |volumes: Vec<Volume>| {
            (
                volumes
                    .into_iter()
                    .chain(std::iter::once(Volume {
                        name: VOLUME_DATA_NAME.to_string(),
                        empty_dir: Some(EmptyDirVolumeSource {
                            medium: None,
                            size_limit: None,
                        }),
                        ..Volume::default()
                    }))
                    .collect(),
                None,
            )
        };

        match self.spec.storage.clone() {
            Some(storage) => {
                if let Some(empty_dir) = storage.empty_dir {
                    (
                        volumes
                            .into_iter()
                            .chain(std::iter::once(Volume {
                                name: VOLUME_DATA_NAME.to_string(),
                                empty_dir: Some(empty_dir),
                                ..Volume::default()
                            }))
                            .collect(),
                        None,
                    )
                } else if let Some(ephemeral) = storage.ephemeral {
                    (
                        volumes
                            .into_iter()
                            .chain(std::iter::once(Volume {
                                name: VOLUME_DATA_NAME.to_string(),
                                ephemeral: Some(ephemeral),
                                ..Volume::default()
                            }))
                            .collect(),
                        None,
                    )
                } else if let Some(volume_claim_template) = storage.volume_claim_template {
                    (volumes, Some(vec![volume_claim_template]))
                } else {
                    default_expand_storage(volumes)
                }
            }
            None => default_expand_storage(volumes),
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
                name: "KANIDM_ORIGIN".to_string(),
                value: Some(format!("https://{}", self.spec.domain.clone())),
                ..EnvVar::default()
            },
            EnvVar {
                name: "KANIDM_DB_PATH".to_string(),
                value: Some(format!("{VOLUME_DATA_PATH}/kanidm.db")),
                ..EnvVar::default()
            },
            EnvVar {
                name: "KANIDM_TLS_CHAIN".to_string(),
                value: Some(format!("{VOLUME_TLS_PATH}/tls.crt")),
                ..EnvVar::default()
            },
            EnvVar {
                name: "KANIDM_TLS_KEY".to_string(),
                value: Some(format!("{VOLUME_TLS_PATH}/tls.key")),
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
                .chain([
                    VolumeMount {
                        name: VOLUME_DATA_NAME.to_string(),
                        mount_path: VOLUME_DATA_PATH.to_string(),
                        ..VolumeMount::default()
                    },
                    VolumeMount {
                        name: VOLUME_TLS_NAME.to_string(),
                        mount_path: VOLUME_TLS_PATH.to_string(),
                        read_only: Some(true),
                        ..VolumeMount::default()
                    },
                ])
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

        let secret_name = self.spec.tls_secret_name.clone().unwrap_or_else(|| {
            self.spec
                .ingress
                .as_ref()
                .and_then(|i| i.tls_secret_name.clone())
                .unwrap_or_else(|| self.get_secret_name())
        });
        let (volumes, volume_claim_templates) = self.expand_storage(
            self.spec
                .volumes
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(std::iter::once(Volume {
                    name: VOLUME_TLS_NAME.to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(secret_name),
                        ..SecretVolumeSource::default()
                    }),
                    ..Volume::default()
                }))
                .collect(),
        );
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
                    metadata: Some(ObjectMeta {
                        labels: Some(pod_labels),
                        ..ObjectMeta::default()
                    }),
                    spec: Some(PodSpec {
                        containers,
                        volumes: Some(volumes),
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

    fn create_kanidm_with_storage(storage: Option<KanidmStorage>) -> Kanidm {
        Kanidm {
            spec: KanidmSpec {
                storage,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_volumes_without_storage() {
        let kanidm = create_kanidm_with_storage(None);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.clone().len(), 1);
        assert_eq!(volumes.clone().first().unwrap().name, "kanidm-data");
        assert!(volumes.first().unwrap().empty_dir.is_some());
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_emptydir() {
        let storage = Some(KanidmStorage {
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.clone().len(), 1);
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.clone().len(), 1);
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.len(), 1);
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.len(), 1);
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.len(), 1);
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert!(volumes.is_empty());
        assert!(volume_claim_template.is_some());
    }

    #[test]
    fn test_generate_volumes_with_existing_volumes() {
        let existing_volume = Volume {
            name: "existing-volume".to_string(),
            ..Volume::default()
        };
        let kanidm = create_kanidm_with_storage(None);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![existing_volume.clone()]);

        assert_eq!(volumes.len(), 2);
        assert!(volumes.clone().iter().any(|v| v.name == "existing-volume"));
        assert!(volumes
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
        let kanidm = create_kanidm_with_storage(None);
        let (volumes, volume_claim_template) =
            kanidm.expand_storage(vec![existing_volume1.clone(), existing_volume2.clone()]);

        assert_eq!(volumes.len(), 3);
        assert!(volumes
            .clone()
            .iter()
            .any(|v| v.name == "existing-volume-1"));
        assert!(volumes
            .clone()
            .iter()
            .any(|v| v.name == "existing-volume-2"));
        assert!(volumes
            .iter()
            .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some()));
        assert!(volume_claim_template.is_none());
    }
}
