use super::secret::{SecretExt, REPLICA_SECRET_KEY};
use super::service::ServiceExt;

use crate::kanidm::crd::{Kanidm, KanidmServerRole, ReplicaGroup, ReplicationType};

use kaniop_k8s_util::resources::merge_containers;

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EmptyDirVolumeSource, EnvVar, EnvVarSource, HTTPGetAction,
    ObjectFieldSelector, PersistentVolumeClaim, PodSpec, PodTemplateSpec, Probe, SecretKeySelector,
    SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{ObjectMeta, Resource};
use kube::ResourceExt;

pub const REPLICA_GROUP_LABEL: &str = "kanidm.kaniop.rs/replica-group";
pub const CONTAINER_REPLICATION_PORT_NAME: &str = "replication";
pub const CONTAINER_REPLICATION_PORT: i32 = 8444;

// renovate: datasource=docker
const REPLICATION_CONFIG_IMAGE: &str = "ghcr.io/rash-sh/rash:2.9.1";
const REPLICATION_CONFIG_SCRIPT: &str = r#"
- copy:
    content: |
      [replication]
      origin = "repl://{{ env.POD_NAME }}:{{ env.REPLICATION_PORT }}"
      bindaddress = "0.0.0.0:{{ env.REPLICATION_PORT }}"

      {% for e in env -%}
      {% if e is startingwith(env.KANIDM_SERVICE_NAME| upper | replace('-', '_')) -%}
      {% if e == env.POD_NAME | upper | replace('-','_') or e is endingwith("_TYPE") or
         e + '_TYPE' not in env or env[e + '_TYPE'] == "" -%}
        {% continue -%}
      {% endif -%}
      {% set replica = e | lower | replace('_', '-') -%}
      [replication."repl://{{ replica }}:{{ env.REPLICATION_PORT }}"]
      {% set type = env[e + '_TYPE'] -%}
      type = "{{ type }}"
      {% if type == "mutual-pull" -%}
      partner_cert = "{{ env[e] }}"
      {% elif type == "pull" -%}
      supplier_cert = "{{ env[e] }}"
      {% else -%}
      consumer_cert = "{{ env[e] }}"
      {% endif -%}
      {% if type != "allow-pull" -%}
      {% if replica == env.KANIDM_PRIMARY_NODE -%}
      automatic_refresh = true
      {% else -%}
      automatic_refresh = false
      {% endif -%}
      {% endif %}
      {% elif e is startingwith("EXTERNAL_REPLICATION_NODE") -%}
      {% if e + '_CERT' not in env or e is endingwith("_TYPE") or e is endingwith("_CERT") or e is endingwith("_AUTOMATIC_REFRESH") -%}
        {% continue -%}
      {% endif -%}
      [replication."{{ env[e] }}"]
      {% set type = env[e + '_TYPE'] -%}
      type = "{{ type }}"
      {% if type == "mutual-pull" -%}
      partner_cert = "{{ env[e + '_CERT'] }}"
      {% elif type == "pull" -%}
      supplier_cert = "{{ env[e + '_CERT'] }}"
      {% else -%}
      consumer_cert = "{{ env[e + '_CERT'] }}"
      {% endif -%}
      {% if type != "allow-pull" -%}
      automatic_refresh = {{ env[e + '_AUTOMATIC_REFRESH'] }}
      {% endif %}
      {% endif %}
      {%- endfor -%}
    dest: "{{ env.KANIDM_CONFIG_PATH }}"
"#;
const CONTAINER_HTTPS_PORT: i32 = 8443;
const CONTAINER_LDAP_PORT: i32 = 3636;
// TODO: change to a shared volume
const KANIDM_CONFIG_PATH: &str = "/data/server.toml";
const VOLUME_DATA_NAME: &str = "kanidm-data";
const VOLUME_DATA_PATH: &str = "/data";
const VOLUME_TLS_NAME: &str = "kanidm-certs";
const VOLUME_TLS_PATH: &str = "/etc/kanidm/tls";

pub trait StatefulSetExt {
    fn statefulset_name(&self, rg_name: &str) -> String;
    fn create_statefulset(&self, replica_group: &ReplicaGroup) -> StatefulSet;
}

trait StatefulSetExtPrivate {
    fn expand_storage(
        &self,
        volumes: Vec<Volume>,
    ) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>);
    fn generate_pod_labels(&self, replica_group: &ReplicaGroup) -> BTreeMap<String, String>;
    fn generate_labels(&self, pod_labels: &BTreeMap<String, String>) -> BTreeMap<String, String>;
    fn generate_env_vars(&self, replica_group: &ReplicaGroup) -> Vec<EnvVar>;
    fn generate_volume_mounts(&self) -> Vec<VolumeMount>;
    #[allow(clippy::ptr_arg)]
    fn generate_init_containers(
        &self,
        volume_mounts: &Vec<VolumeMount>,
        replica_group: &ReplicaGroup,
    ) -> Vec<Container>;
    fn generate_container_ports(&self) -> Vec<ContainerPort>;
    fn generate_probe(&self) -> Probe;
    #[allow(clippy::ptr_arg)]
    fn generate_containers(
        &self,
        env: &Vec<EnvVar>,
        volume_mounts: &Vec<VolumeMount>,
        ports: &Vec<ContainerPort>,
        probe: &Probe,
        replica_group: &ReplicaGroup,
    ) -> Vec<Container>;
    fn generate_dns_policy(&self) -> Option<String>;
    fn generate_volumes(&self) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>);
    fn generate_metadata(
        &self,
        labels: &BTreeMap<String, String>,
        replica_group_name: &str,
    ) -> ObjectMeta;
}

impl StatefulSetExt for Kanidm {
    #[inline]
    fn statefulset_name(&self, rg_name: &str) -> String {
        format!("{kanidm_name}-{rg_name}", kanidm_name = self.name_any())
    }

    fn create_statefulset(&self, replica_group: &ReplicaGroup) -> StatefulSet {
        let pod_labels = self.generate_pod_labels(replica_group);
        let labels = self.generate_labels(&pod_labels);
        let env = self.generate_env_vars(replica_group);
        let volume_mounts = self.generate_volume_mounts();
        let init_containers = self.generate_init_containers(&volume_mounts, replica_group);
        let ports = self.generate_container_ports();
        let probe = self.generate_probe();
        let containers =
            self.generate_containers(&env, &volume_mounts, &ports, &probe, replica_group);
        let dns_policy = self.generate_dns_policy();
        let (volumes, volume_claim_templates) = self.generate_volumes();

        StatefulSet {
            metadata: self.generate_metadata(&labels, &replica_group.name),
            spec: Some(StatefulSetSpec {
                replicas: Some(replica_group.replicas),
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
                        node_selector: replica_group.node_selector.clone(),
                        affinity: replica_group.affinity.clone(),
                        tolerations: replica_group.tolerations.clone(),
                        topology_spread_constraints: replica_group
                            .topology_spread_constraints
                            .clone(),
                        security_context: self.spec.security_context.clone(),
                        dns_policy,
                        dns_config: self.spec.dns_config.clone(),
                        init_containers: Some(init_containers),
                        host_aliases: self.spec.host_aliases.clone(),
                        enable_service_links: Some(false),
                        ..PodSpec::default()
                    }),
                },
                service_name: self.service_name(),
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

impl StatefulSetExtPrivate for Kanidm {
    fn generate_pod_labels(&self, replica_group: &ReplicaGroup) -> BTreeMap<String, String> {
        self.generate_resource_labels()
            .into_iter()
            .chain(std::iter::once((
                REPLICA_GROUP_LABEL.to_string(),
                replica_group.name.clone(),
            )))
            .collect()
    }

    fn generate_labels(&self, pod_labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        self.labels()
            .clone()
            .into_iter()
            .chain(pod_labels.clone())
            .collect()
    }

    fn generate_env_vars(&self, replica_group: &ReplicaGroup) -> Vec<EnvVar> {
        self.spec
            .env
            .clone()
            .unwrap_or_default()
            .into_iter()
            .chain(vec![
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
                    name: "KANIDM_ROLE".to_string(),
                    value: Some(serde_plain::to_string(&replica_group.role.clone()).unwrap()),
                    ..EnvVar::default()
                },
                EnvVar {
                    name: "KANIDM_LOG_LEVEL".to_string(),
                    value: Some(serde_plain::to_string(&self.spec.log_level.clone()).unwrap()),
                    ..EnvVar::default()
                },
            ])
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
            .collect()
    }

    fn generate_volume_mounts(&self) -> Vec<VolumeMount> {
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
            .collect()
    }

    fn generate_init_containers(
        &self,
        volume_mounts: &Vec<VolumeMount>,
        replica_group: &ReplicaGroup,
    ) -> Vec<Container> {
        if self.is_replication_enabled() {
            let external_replica_nodes_envs = self
                .spec
                .external_replication_nodes
                .iter()
                .flat_map(|ern| {
                    [
                        EnvVar {
                            name: format!("EXTERNAL_REPLICATION_NODE_{}", ern.name),
                            value: Some(format!(
                                "repl://{host}:{port}",
                                host = ern.hostname.clone(),
                                port = ern.port
                            )),
                            ..EnvVar::default()
                        },
                        EnvVar {
                            name: format!("EXTERNAL_REPLICATION_NODE_{}_CERT", ern.name),
                            value_from: Some(EnvVarSource {
                                secret_key_ref: Some(ern.certificate.clone()),
                                ..EnvVarSource::default()
                            }),
                            ..EnvVar::default()
                        },
                        EnvVar {
                            name: format!("EXTERNAL_REPLICATION_NODE_{}_TYPE", ern.name),
                            value: serde_plain::to_string(&ern._type).ok(),
                            ..EnvVar::default()
                        },
                        EnvVar {
                            name: format!(
                                "EXTERNAL_REPLICATION_NODE_{}_AUTOMATIC_REFRESH",
                                ern.name
                            ),
                            value: Some(ern.automatic_refresh.to_string()),
                            ..EnvVar::default()
                        },
                    ]
                })
                .collect::<Vec<EnvVar>>();
            let replica_secrets_envs = self
                .spec
                .replica_groups
                .iter()
                .flat_map(|rg| {
                    let sts_name = self.statefulset_name(&rg.name);
                    (0..rg.replicas).flat_map(move |i| {
                        let pod_name = format!("{sts_name}-{i}");
                        let name = pod_name.to_uppercase().replace("-", "_");
                        [
                            EnvVar {
                                name: name.clone(),
                                value_from: Some(EnvVarSource {
                                    secret_key_ref: Some(SecretKeySelector {
                                        name: self.replica_secret_name(&pod_name),
                                        key: REPLICA_SECRET_KEY.to_string(),
                                        optional: Some(true),
                                    }),
                                    ..EnvVarSource::default()
                                }),
                                ..EnvVar::default()
                            },
                            EnvVar {
                                name: format!("{name}_TYPE"),
                                value: replication_type(
                                    replica_group.role.clone(),
                                    rg.role.clone(),
                                )
                                .and_then(|t| serde_plain::to_string(&t).ok()),
                                ..EnvVar::default()
                            },
                        ]
                    })
                })
                .collect::<Vec<EnvVar>>();

            let primary_node = self
                .spec
                .replica_groups
                .iter()
                .find(|rg| rg.primary_node)
                .map(|rg| format!("{}-0", self.statefulset_name(&rg.name)));
            let env = external_replica_nodes_envs
                .into_iter()
                .chain(replica_secrets_envs)
                .chain([
                    EnvVar {
                        name: "POD_NAME".to_string(),
                        value_from: Some(EnvVarSource {
                            field_ref: Some(ObjectFieldSelector {
                                api_version: Some("v1".to_string()),
                                field_path: "metadata.name".to_string(),
                            }),
                            ..EnvVarSource::default()
                        }),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "REPLICATION_PORT".to_string(),
                        value: Some(CONTAINER_REPLICATION_PORT.to_string()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIDM_CONFIG_PATH".to_string(),
                        value: Some(KANIDM_CONFIG_PATH.to_string()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIDM_SERVICE_NAME".to_string(),
                        value: Some(self.service_name()),
                        ..EnvVar::default()
                    },
                ])
                .chain(primary_node.map(|pn| EnvVar {
                    name: "KANIDM_PRIMARY_NODE".to_string(),
                    value: Some(pn),
                    ..EnvVar::default()
                }))
                .collect::<Vec<EnvVar>>();

            let init_container = Container {
                name: "kanidm-generate-replication-config".to_string(),
                image: Some(REPLICATION_CONFIG_IMAGE.to_string()),
                env: Some(env),
                args: Some(vec![
                    "--script".to_string(),
                    REPLICATION_CONFIG_SCRIPT.to_string(),
                ]),
                volume_mounts: Some(volume_mounts.clone()),
                ..Container::default()
            };

            merge_containers(self.spec.init_containers.clone(), &init_container)
        } else {
            self.spec.init_containers.clone().unwrap_or_default()
        }
    }

    fn generate_container_ports(&self) -> Vec<ContainerPort> {
        std::iter::once(ContainerPort {
            name: Some(self.spec.port_name.clone()),
            container_port: CONTAINER_HTTPS_PORT,
            ..ContainerPort::default()
        })
        .chain(
            self.spec
                .ldap_port_name
                .clone()
                .into_iter()
                .map(|port_name| ContainerPort {
                    name: Some(port_name.clone()),
                    container_port: CONTAINER_LDAP_PORT,
                    ..ContainerPort::default()
                }),
        )
        .chain(self.is_replication_enabled().then(|| ContainerPort {
            name: Some(CONTAINER_REPLICATION_PORT_NAME.to_string()),
            container_port: CONTAINER_REPLICATION_PORT,
            ..ContainerPort::default()
        }))
        .collect()
    }

    fn generate_probe(&self) -> Probe {
        Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/status".to_string()),
                port: IntOrString::String(self.spec.port_name.clone()),
                scheme: Some("HTTPS".to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        }
    }

    fn generate_containers(
        &self,
        env: &Vec<EnvVar>,
        volume_mounts: &Vec<VolumeMount>,
        ports: &Vec<ContainerPort>,
        probe: &Probe,
        replica_group: &ReplicaGroup,
    ) -> Vec<Container> {
        let kanidm_container = Container {
            name: "kanidm".to_string(),
            image: Some(self.spec.image.clone()),
            image_pull_policy: self.spec.image_pull_policy.clone(),
            env: Some(env.clone()),
            ports: Some(ports.clone()),
            volume_mounts: Some(volume_mounts.clone()),
            resources: replica_group.resources.clone(),
            readiness_probe: Some(probe.clone()),
            liveness_probe: Some(probe.clone()),
            ..Container::default()
        };

        merge_containers(self.spec.containers.clone(), &kanidm_container)
    }

    fn generate_dns_policy(&self) -> Option<String> {
        match self.spec.host_network {
            Some(true) => Some("ClusterFirstWithHostNet".to_string()),
            _ => self.spec.dns_policy.clone(),
        }
    }

    fn generate_volumes(&self) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>) {
        let secret_name = self.spec.tls_secret_name.clone().unwrap_or_else(|| {
            self.spec
                .ingress
                .as_ref()
                .and_then(|i| i.tls_secret_name.clone())
                .unwrap_or_else(|| self.get_tls_secret_name())
        });

        self.expand_storage(
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
        )
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
                    let named_template = PersistentVolumeClaim {
                        metadata: ObjectMeta {
                            name: Some(VOLUME_DATA_NAME.to_string()),
                            ..volume_claim_template.metadata
                        },
                        spec: volume_claim_template.spec,
                        ..volume_claim_template
                    };
                    (volumes, Some(vec![named_template]))
                } else {
                    default_expand_storage(volumes)
                }
            }
            None => default_expand_storage(volumes),
        }
    }

    fn generate_metadata(
        &self,
        labels: &BTreeMap<String, String>,
        replica_group_name: &str,
    ) -> ObjectMeta {
        ObjectMeta {
            name: Some(self.statefulset_name(replica_group_name)),
            namespace: self.namespace(),
            labels: Some(labels.clone()),
            owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
            annotations: Some(self.annotations().to_owned()),
            ..ObjectMeta::default()
        }
    }
}

fn replication_type(
    source_role: KanidmServerRole,
    target_role: KanidmServerRole,
) -> Option<ReplicationType> {
    match (source_role, target_role) {
        (
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUI,
            KanidmServerRole::WriteReplicaNoUI | KanidmServerRole::WriteReplica,
        ) => Some(ReplicationType::MutualPull),

        (
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUI,
            KanidmServerRole::ReadOnlyReplica,
        ) => Some(ReplicationType::AllowPull),
        (
            KanidmServerRole::ReadOnlyReplica,
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUI,
        ) => Some(ReplicationType::Pull),
        (KanidmServerRole::ReadOnlyReplica, KanidmServerRole::ReadOnlyReplica) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::StatefulSetExtPrivate;

    use crate::kanidm::crd::{Kanidm, KanidmSpec, KanidmStorage};
    use k8s_openapi::api::core::v1::{
        EmptyDirVolumeSource, EphemeralVolumeSource, PersistentVolumeClaim, Volume,
    };

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

#[cfg(all(test, feature = "integration-test"))]
mod integration_test {
    use super::{REPLICATION_CONFIG_IMAGE, REPLICATION_CONFIG_SCRIPT};

    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;
    use testcontainers::core::Mount;
    use testcontainers::runners::AsyncRunner;
    use testcontainers::ContainerRequest;
    use testcontainers::GenericImage;
    use testcontainers::ImageExt;
    use tokio::io::{AsyncBufReadExt, BufReader};

    struct TestCase<'a> {
        env_vars: Vec<(&'a str, &'a str)>,
        expected_result: &'a str,
    }

    async fn run_test_case(
        image_parts: &[&str],
        cmd: &[&str],
        tmp_dir_path: &str,
        env_vars: &[(&str, &str)],
        expected_result: &str,
    ) {
        let container = GenericImage::new(image_parts[0], image_parts[1]);
        let mut container_request: ContainerRequest<GenericImage> = container.clone().into();

        for (key, value) in env_vars {
            container_request =
                container_request.with_env_var((*key).to_string(), (*value).to_string());
        }

        let container = container_request
            .with_cmd(cmd.iter().map(|&s| s.to_string()))
            .with_mount(Mount::bind_mount(tmp_dir_path.to_string(), "/tmp"))
            .start()
            .await
            .unwrap();

        let stdout = container.stdout(true);
        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stdout_lines = Vec::new();
        while let Some(l) = stdout_reader.next_line().await.unwrap() {
            stdout_lines.push(l);
        }
        dbg!(stdout_lines.join("\n"));

        let stderr = container.stderr(true);
        let mut stderr_reader = BufReader::new(stderr).lines();
        let mut stderr_lines = Vec::new();
        while let Some(l) = stderr_reader.next_line().await.unwrap() {
            stderr_lines.push(l);
        }
        dbg!(stderr_lines.join("\n"));

        let server_toml_path = Path::new(tmp_dir_path).join("server.toml");
        let content = fs::read_to_string(server_toml_path).expect("Unable to read server.toml");
        assert_eq!(content, expected_result);
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[tokio::test]
    async fn test_replication_config_generation() {
        let image_parts = REPLICATION_CONFIG_IMAGE.split(':').collect::<Vec<&str>>();
        let cmd = ["--script", REPLICATION_CONFIG_SCRIPT];
        let tmp_dir = tempdir().unwrap();
        let tmp_dir_path = tmp_dir.path().to_str().unwrap().to_string();

        let test_cases = vec![
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-0:8444"
bindaddress = "0.0.0.0:8444"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0",
                        "repl://external-host-0:8444",
                    ),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0_CERT",
                        "dummy-cert-external-host-0",
                    ),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_TYPE", "mutual-pull"),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_AUTOMATIC_REFRESH", "true"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-0:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://external-host-0:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-external-host-0"
automatic_refresh = true

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0",
                        "repl://external-host-0:8444",
                    ),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_TYPE", "mutual-pull"),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_AUTOMATIC_REFRESH", "true"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-0:8444"
bindaddress = "0.0.0.0:8444"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-0:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-1:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-3"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-1"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-1:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-3:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-3"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-3"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-default-3:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("REPLICA_GROUP", "read-replica"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-read-replica-0"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-read-replica-0:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("REPLICA_GROUP", "read-replica"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-read-replica-1"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-read-replica-1:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("KANIDM_SERVICE_NAME", "kanidm-test"),
                    ("REPLICA_GROUP", "read-replica"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-read-replica-1"),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0",
                        "repl://external-host-0:8444",
                    ),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0_CERT",
                        "dummy-cert-external-host-0",
                    ),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_TYPE", "mutual-pull"),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_AUTOMATIC_REFRESH", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                ],
                expected_result: r#"[replication]
origin = "repl://kanidm-test-read-replica-1:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://external-host-0:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-external-host-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-0:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
        ];

        for test_case in test_cases {
            run_test_case(
                &image_parts,
                &cmd,
                &tmp_dir_path,
                &test_case.env_vars,
                test_case.expected_result,
            )
            .await;
        }
    }
}
