use super::secret::{REPLICA_SECRET_KEY, SecretExt};
use super::service::ServiceExt;
use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};

use crate::controller::cluster_domain;
use crate::kanidm::crd::{IpFamily, Kanidm, KanidmServerRole, ReplicaGroup, ReplicationType};

use kaniop_backup_core::auth::{
    AuthRole, build_auth_env_vars, build_auth_volume_mounts, build_auth_volumes,
    build_ca_bundle_volume, build_ca_bundle_volume_mount, build_encryption_env_vars,
    ca_bundle_env_var,
};
use kaniop_backup_core::image::data_mover_image;
use kaniop_backup_core::pod_defaults::{default_resource_requirements, hardened_security_context};

use kaniop_k8s_util::error::Result;
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
use kube::ResourceExt;
use kube::api::{ObjectMeta, Resource};

pub const REPLICA_GROUP_LABEL: &str = "kanidm.kaniop.rs/replica-group";
pub const REPLICA_LABEL: &str = "kanidm.kaniop.rs/replica";
pub const TLS_SECRET_HASH_ANNOTATION: &str = "kanidm.kaniop.rs/tls-secret-hash";
pub const CONTAINER_REPLICATION_PORT: i32 = 8444;
pub const CONTAINER_REPLICATION_PORT_NAME: &str = "replication";

// renovate: datasource=docker
const REPLICATION_CONFIG_IMAGE: &str = "ghcr.io/rash-sh/rash:2.21.0";
const REPLICATION_CONFIG_SCRIPT: &str = r#"
- copy:
    content: |
      {% set backup_enabled = env.KANIOP_BACKUP_ENABLED if env.KANIOP_BACKUP_ENABLED is defined else "" %}
      {% set primary_node = env.KANIDM_PRIMARY_NODE if env.KANIDM_PRIMARY_NODE is defined else "" %}
      {% set backup_schedule = env.KANIOP_BACKUP_SCHEDULE if env.KANIOP_BACKUP_SCHEDULE is defined else "" %}
      {% set backup_versions = env.KANIOP_BACKUP_VERSIONS if env.KANIOP_BACKUP_VERSIONS is defined else "" %}
      version = "2"

      {% if backup_enabled == "true" and env.POD_NAME == primary_node -%}
      [online_backup]
      path = "/data/backups"
      schedule = "{{ backup_schedule }}"
      versions = {{ backup_versions }}
      {% endif -%}

      {% if env.KANIOP_REPLICATION_ENABLED == "true" -%}
      {% set pod_env = env.POD_NAME | upper | replace('-', '_') -%}
      [replication]
      origin = "repl://{{ env[pod_env + '_HOST'] }}:{{ env.REPLICATION_PORT }}"
      bindaddress = "{{ env.BIND_ADDRESS }}:{{ env.REPLICATION_PORT }}"

      {% for e in env -%}
      {% if e is startingwith(env.KANIDM_NAME| upper | replace('-', '_')) -%}
      {% if e == pod_env or e is endingwith("_TYPE") or
         e + '_TYPE' not in env or env[e + '_TYPE'] == "" -%}
        {% continue -%}
      {% endif -%}
      {% set replica = e | lower | replace('_', '-') -%}
      [replication."repl://{{ env[e + '_HOST'] }}:{{ env.REPLICATION_PORT }}"]
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
      {% if replica == primary_node -%}
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
      {% endif -%}
    dest: "{{ env.KANIDM_CONFIG_PATH }}"
    mode: "0400"
"#;
const CONTAINER_HTTPS_PORT: i32 = 8443;
const CONTAINER_LDAP_PORT: i32 = 3636;
pub const KANIDM_CONFIG_PATH: &str = "/run/kanidm/server.toml";
pub const KANIDM_BACKUP_PATH: &str = "/data/backups";
const VOLUME_CONFIG_NAME: &str = "kanidm-config";
const VOLUME_CONFIG_PATH: &str = "/run/kanidm";
const VOLUME_DATA_NAME: &str = "kanidm-data";
const VOLUME_DATA_PATH: &str = "/data";
const VOLUME_TLS_NAME: &str = "kanidm-certs";
const VOLUME_TLS_PATH: &str = "/etc/kanidm/tls";

const IPV4_BIND_ADDRESS: &str = "0.0.0.0";
const IPV6_BIND_ADDRESS: &str = "[::]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StatefulSetApplyStrategy {
    Apply,
    Recreate { immutable_fields: Vec<&'static str> },
}

/// Normalize only API defaults with stable, resource-local semantics.
///
/// * `podManagementPolicy` defaults to `OrderedReady`.
/// * PVC `volumeMode` defaults to `Filesystem`.
/// * When the generated desired PVC template has `storageClassName: None` (omission or
///   defaulting), preserve the live object's `storageClassName` so that a CR cannot
///   distinguish omission/defaulting from removal after the StatefulSet has been created.
///
/// Never copies `storageClassName` when the desired object explicitly sets a non-None
/// value—the explicit desired value always wins.
pub(super) fn preserve_defaulted_statefulset_fields(
    desired: &mut StatefulSet,
    current: &StatefulSet,
) {
    let Some(desired_spec) = desired.spec.as_mut() else {
        return;
    };

    if desired_spec.pod_management_policy.is_none() {
        desired_spec.pod_management_policy = Some("OrderedReady".to_string());
    }

    if let Some(templates) = desired_spec.volume_claim_templates.as_mut() {
        if let Some(current_templates) = current
            .spec
            .as_ref()
            .and_then(|s| s.volume_claim_templates.as_ref())
        {
            for template in templates.iter_mut() {
                if let Some(spec) = template.spec.as_mut() {
                    // Preserve defaulted storageClassName: the CR cannot distinguish omission
                    // from removal after creation, so compatibility/defaulting wins. Match by
                    // claim-template name rather than relying on vector order.
                    if spec.storage_class_name.is_none()
                        && let Some(template_name) = template.metadata.name.as_deref()
                        && let Some(current_spec) = current_templates
                            .iter()
                            .find(|current| current.metadata.name.as_deref() == Some(template_name))
                            .and_then(|current| current.spec.as_ref())
                    {
                        spec.storage_class_name
                            .clone_from(&current_spec.storage_class_name);
                    }
                    if spec.volume_mode.is_none() {
                        spec.volume_mode = Some("Filesystem".to_string());
                    }
                }
            }
        } else {
            for template in templates {
                if let Some(spec) = template.spec.as_mut() {
                    if spec.volume_mode.is_none() {
                        spec.volume_mode = Some("Filesystem".to_string());
                    }
                }
            }
        }
    }
}

fn pod_management_policy(spec: &StatefulSetSpec) -> &str {
    spec.pod_management_policy
        .as_deref()
        .unwrap_or("OrderedReady")
}

fn normalized_volume_claim_templates(spec: &StatefulSetSpec) -> Option<Vec<PersistentVolumeClaim>> {
    let mut templates = spec.volume_claim_templates.clone();
    if let Some(templates) = templates.as_mut() {
        for template in templates {
            if let Some(pvc_spec) = template.spec.as_mut()
                && pvc_spec.volume_mode.is_none()
            {
                pvc_spec.volume_mode = Some("Filesystem".to_string());
            }
        }
    }
    templates
}

fn deletes_pvcs_when_deleted(spec: &StatefulSetSpec) -> bool {
    spec.persistent_volume_claim_retention_policy
        .as_ref()
        .and_then(|policy| policy.when_deleted.as_deref())
        == Some("Delete")
}

/// Classify StatefulSet updates without deriving destructive behavior from an HTTP status code.
///
/// Selector, serviceName and podManagementPolicy changes can be recreated when deletion is known
/// to preserve PVCs. `volumeClaimTemplates` changes are deliberately left on the non-destructive
/// apply path because deleting/recreating a StatefulSet is not a PVC migration. Kubernetes will
/// reject unsupported template updates while the existing StatefulSet remains intact.
pub(super) fn classify_statefulset_change(
    current: &StatefulSet,
    desired: &StatefulSet,
) -> StatefulSetApplyStrategy {
    let (Some(current_spec), Some(desired_spec)) = (current.spec.as_ref(), desired.spec.as_ref())
    else {
        // A malformed or incomplete object is not sufficient evidence for deletion.
        return StatefulSetApplyStrategy::Apply;
    };

    if normalized_volume_claim_templates(current_spec)
        != normalized_volume_claim_templates(desired_spec)
    {
        return StatefulSetApplyStrategy::Apply;
    }

    let mut immutable_fields = Vec::new();
    if current_spec.selector != desired_spec.selector {
        immutable_fields.push("spec.selector");
    }
    if current_spec.service_name != desired_spec.service_name {
        immutable_fields.push("spec.serviceName");
    }
    if pod_management_policy(current_spec) != pod_management_policy(desired_spec) {
        immutable_fields.push("spec.podManagementPolicy");
    }

    if immutable_fields.is_empty() || deletes_pvcs_when_deleted(current_spec) {
        StatefulSetApplyStrategy::Apply
    } else {
        StatefulSetApplyStrategy::Recreate { immutable_fields }
    }
}

pub trait StatefulSetExt {
    fn statefulset_name(&self, rg_name: &str) -> String;
    fn pod_name(&self, rg_name: &str, i: i32) -> String;
    fn pod_env_prefix(&self, pod_name: &str) -> String;

    fn create_statefulset(
        &self,
        replica_group: &ReplicaGroup,
        tls_secret_hash: Option<&str>,
        backup_config: Option<&BackupConfig>,
    ) -> Result<StatefulSet>;
}

impl StatefulSetExt for Kanidm {
    #[inline]
    fn statefulset_name(&self, rg_name: &str) -> String {
        format!("{kanidm_name}-{rg_name}", kanidm_name = self.name_any())
    }

    #[inline]
    fn pod_name(&self, rg_name: &str, i: i32) -> String {
        format!("{}-{}", self.statefulset_name(rg_name), i)
    }

    #[inline]
    fn pod_env_prefix(&self, pod_name: &str) -> String {
        pod_name.to_uppercase().replace("-", "_")
    }

    fn create_statefulset(
        &self,
        replica_group: &ReplicaGroup,
        tls_secret_hash: Option<&str>,
        backup_config: Option<&BackupConfig>,
    ) -> Result<StatefulSet> {
        let pod_labels = self.generate_pod_labels(replica_group);
        let labels = self.generate_sts_labels(&pod_labels);
        let env = self.generate_env_vars(replica_group);
        let mut init_containers = self.generate_init_containers(replica_group, backup_config)?;
        let ports = self.generate_container_ports();
        let probe = self.generate_probe();
        let volume_mounts = self.generate_volume_mounts(backup_config);
        let mut containers = self.generate_containers(
            &env,
            &volume_mounts,
            &ports,
            &probe,
            replica_group,
            backup_config,
        )?;
        let transport_config = backup_config.and_then(|config| config.transport.as_ref());
        let transport_name = super::transport::TRANSPORT_SIDECAR_NAME;

        // The transport is operator-managed. Remove any stale regular-container placement from
        // the desired pod and any same-name user init container before injecting the native
        // sidecar. This also makes upgrades from the previous topology deterministic.
        containers.retain(|c| c.name != transport_name);
        init_containers.retain(|c| c.name != transport_name);
        if replica_group.primary_node {
            if let Some(config) = transport_config {
                init_containers.push(self.build_transport_sidecar(config, replica_group)?);
            }
        }

        let dns_policy = self.generate_dns_policy();
        let (mut volumes, volume_claim_templates) = self.generate_volumes(backup_config);

        if let Some(config) = transport_config {
            let auth_volumes = build_auth_volumes(&config.auth_method);
            volumes.extend(auth_volumes);
            if let Some(ca_bundle_ref) = &config.ca_bundle_ref {
                volumes.push(build_ca_bundle_volume(ca_bundle_ref));
            }
            if !volumes.iter().any(|v| v.name == "kanidm-tmp") {
                volumes.push(Volume {
                    name: "kanidm-tmp".to_string(),
                    empty_dir: Some(EmptyDirVolumeSource::default()),
                    ..Volume::default()
                });
            }
        }

        Ok(StatefulSet {
            metadata: self.generate_metadata(
                &replica_group.name,
                &replica_group.stateful_set_annotations,
                &labels,
            ),
            spec: Some(StatefulSetSpec {
                replicas: Some(replica_group.replicas),
                selector: LabelSelector {
                    match_expressions: None,
                    match_labels: Some(pod_labels.clone()),
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(pod_labels),
                        annotations: tls_secret_hash.map(|hash| {
                            BTreeMap::from([(
                                TLS_SECRET_HASH_ANNOTATION.to_string(),
                                hash.to_string(),
                            )])
                        }),
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
                        enable_service_links: Some(self.spec.enable_service_links),
                        automount_service_account_token: self.spec.automount_service_account_token,
                        host_users: self.spec.host_users,
                        runtime_class_name: self.spec.runtime_class_name.clone(),
                        ..PodSpec::default()
                    }),
                },
                service_name: Some(self.service_name()),
                persistent_volume_claim_retention_policy: self
                    .spec
                    .persistent_volume_claim_retention_policy
                    .clone(),
                min_ready_seconds: self.spec.min_ready_seconds,
                volume_claim_templates,
                ..StatefulSetSpec::default()
            }),
            ..StatefulSet::default()
        })
    }
}

impl Kanidm {
    fn uses_generated_config(&self, backup_config: Option<&BackupConfig>) -> bool {
        self.is_replication_enabled() || backup_config.is_some()
    }

    fn generate_pod_labels(&self, replica_group: &ReplicaGroup) -> BTreeMap<String, String> {
        self.generate_resource_labels()
            .into_iter()
            .chain(std::iter::once((
                REPLICA_GROUP_LABEL.to_string(),
                replica_group.name.clone(),
            )))
            .collect()
    }

    fn generate_sts_labels(
        &self,
        pod_labels: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        pod_labels.clone()
    }

    fn generate_env_vars(&self, replica_group: &ReplicaGroup) -> Vec<EnvVar> {
        let origin = match self.spec.origin.clone() {
            Some(o) => o,
            None => format!("https://{}", self.spec.domain.clone()),
        };
        let bind_address = match self.spec.ip_family {
            IpFamily::Ipv4 => IPV4_BIND_ADDRESS,
            IpFamily::Ipv6 => IPV6_BIND_ADDRESS,
        };

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
                    value: Some(origin),
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
                    value: Some(format!("{bind_address}:{CONTAINER_HTTPS_PORT}")),
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
                        value: Some(format!("{bind_address}:{CONTAINER_LDAP_PORT}")),
                        ..EnvVar::default()
                    }),
            )
            .collect()
    }

    fn generate_config_volume_mount(&self) -> VolumeMount {
        VolumeMount {
            name: VOLUME_CONFIG_NAME.to_string(),
            mount_path: VOLUME_CONFIG_PATH.to_string(),
            read_only: Some(false),
            ..VolumeMount::default()
        }
    }

    fn generate_volume_mounts(&self, backup_config: Option<&BackupConfig>) -> Vec<VolumeMount> {
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
            .chain(
                self.uses_generated_config(backup_config)
                    .then(|| self.generate_config_volume_mount()),
            )
            .collect()
    }

    fn generate_init_containers(
        &self,
        replica_group: &ReplicaGroup,
        backup_config: Option<&BackupConfig>,
    ) -> Result<Vec<Container>> {
        if self.uses_generated_config(backup_config) {
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
                    (0..rg.replicas).flat_map(move |i| {
                        let pod_name = self.pod_name(&rg.name, i);
                        let pod_env_prefix = self.pod_env_prefix(&pod_name);
                        let pod_host = match rg
                            .services
                            .as_ref()
                            .and_then(|s| s.replication_hostname_template.as_ref())
                        {
                            Some(template) => template
                                .replace("{pod_name}", &pod_name)
                                .replace("{replica_index}", &i.to_string())
                                .replace("{domain}", &self.spec.domain),
                            None => format!(
                                "{pod_name}.{}.{}.svc.{}",
                                self.service_name(),
                                self.get_namespace(),
                                cluster_domain()
                            ),
                        };
                        [
                            EnvVar {
                                name: pod_env_prefix.clone(),
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
                                name: format!("{pod_env_prefix}_TYPE"),
                                value: replication_type(
                                    replica_group.role.clone(),
                                    rg.role.clone(),
                                )
                                .and_then(|t| serde_plain::to_string(&t).ok()),
                                ..EnvVar::default()
                            },
                            EnvVar {
                                name: format!("{pod_env_prefix}_HOST"),
                                value: Some(pod_host),
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

            let bind_address = match self.spec.ip_family {
                IpFamily::Ipv4 => IPV4_BIND_ADDRESS,
                IpFamily::Ipv6 => IPV6_BIND_ADDRESS,
            };

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
                        name: "BIND_ADDRESS".to_string(),
                        value: Some(bind_address.to_string()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIDM_CONFIG_PATH".to_string(),
                        value: Some(KANIDM_CONFIG_PATH.to_string()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIDM_NAME".to_string(),
                        value: Some(self.name_any()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIOP_REPLICATION_ENABLED".to_string(),
                        value: Some(self.is_replication_enabled().to_string()),
                        ..EnvVar::default()
                    },
                    EnvVar {
                        name: "KANIOP_BACKUP_ENABLED".to_string(),
                        value: Some(backup_config.is_some().to_string()),
                        ..EnvVar::default()
                    },
                ])
                .chain(backup_config.into_iter().flat_map(|backup| {
                    [
                        EnvVar {
                            name: "KANIOP_BACKUP_SCHEDULE".to_string(),
                            value: Some(backup.schedule.clone()),
                            ..EnvVar::default()
                        },
                        EnvVar {
                            name: "KANIOP_BACKUP_VERSIONS".to_string(),
                            value: Some(backup.local_versions.to_string()),
                            ..EnvVar::default()
                        },
                    ]
                }))
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
                volume_mounts: Some(vec![self.generate_config_volume_mount()]),
                ..Container::default()
            };

            merge_containers(self.spec.init_containers.clone(), &init_container)
        } else {
            // When replication is disabled, filter out any user init containers that have the
            // name kanidm-generate-replication-config since that's an operator-managed container
            // that only exists with replication enabled
            let filtered_init_containers = self
                .spec
                .init_containers
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|container| container.name != "kanidm-generate-replication-config")
                .collect();
            Ok(filtered_init_containers)
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
        env: &[EnvVar],
        volume_mounts: &[VolumeMount],
        ports: &[ContainerPort],
        probe: &Probe,
        replica_group: &ReplicaGroup,
        backup_config: Option<&BackupConfig>,
    ) -> Result<Vec<Container>> {
        let command = vec!["kanidmd".to_string(), "server".to_string()]
            .into_iter()
            .chain(
                self.uses_generated_config(backup_config)
                    .then(|| vec!["-c".to_string(), KANIDM_CONFIG_PATH.to_string()])
                    .into_iter()
                    .flatten(),
            )
            .collect::<Vec<String>>();
        let kanidm_container = Container {
            name: "kanidm".to_string(),
            image: Some(self.spec.image.clone()),
            image_pull_policy: self.spec.image_pull_policy.clone(),
            command: Some(command),
            env: Some(env.to_owned()),
            ports: Some(ports.to_owned()),
            volume_mounts: Some(volume_mounts.to_owned()),
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

    fn generate_volumes(
        &self,
        backup_config: Option<&BackupConfig>,
    ) -> (Vec<Volume>, Option<Vec<PersistentVolumeClaim>>) {
        let secret_name = self.effective_tls_secret_name();

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
                        default_mode: Some(0o400),
                        ..SecretVolumeSource::default()
                    }),
                    ..Volume::default()
                }))
                .chain(self.uses_generated_config(backup_config).then(|| Volume {
                    name: VOLUME_CONFIG_NAME.to_string(),
                    empty_dir: Some(EmptyDirVolumeSource {
                        medium: None,
                        size_limit: None,
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
                    let pvc = volume_claim_template.to_persistent_volume_claim();
                    let named_template = PersistentVolumeClaim {
                        metadata: ObjectMeta {
                            name: Some(VOLUME_DATA_NAME.to_string()),
                            ..pvc.metadata
                        },
                        spec: pvc.spec,
                        ..pvc
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
        replica_group_name: &str,
        annotations: &Option<BTreeMap<String, String>>,
        labels: &BTreeMap<String, String>,
    ) -> ObjectMeta {
        ObjectMeta {
            name: Some(self.statefulset_name(replica_group_name)),
            namespace: self.namespace(),
            labels: Some(labels.clone()),
            owner_references: self.controller_owner_ref(&()).map(|oref| vec![oref]),
            annotations: annotations.clone(),
            ..ObjectMeta::default()
        }
    }

    fn build_transport_sidecar(
        &self,
        config: &TransportSidecarConfig,
        replica_group: &ReplicaGroup,
    ) -> Result<Container> {
        let primary_node = format!("{}-0", self.statefulset_name(&replica_group.name));

        let mut env_vars = vec![
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
                name: "KANIDM_PRIMARY_NODE".to_string(),
                value: Some(primary_node),
                ..EnvVar::default()
            },
        ];

        let auth_env = build_auth_env_vars(&config.auth_method, &self.name_any(), AuthRole::Writer);
        env_vars.extend(auth_env);

        if config.ca_bundle_ref.is_some() {
            env_vars.push(ca_bundle_env_var());
        }

        env_vars.extend(build_encryption_env_vars(
            config.encryption_key_ref.as_ref(),
        ));

        let mut volume_mounts = vec![
            VolumeMount {
                name: VOLUME_DATA_NAME.to_string(),
                mount_path: VOLUME_DATA_PATH.to_string(),
                read_only: Some(true),
                ..VolumeMount::default()
            },
            VolumeMount {
                name: "kanidm-tmp".to_string(),
                mount_path: "/tmp".to_string(),
                ..VolumeMount::default()
            },
        ];

        let auth_mounts = build_auth_volume_mounts(&config.auth_method);
        volume_mounts.extend(auth_mounts);

        if config.ca_bundle_ref.is_some() {
            volume_mounts.push(build_ca_bundle_volume_mount());
        }

        Ok(Container {
            name: super::transport::TRANSPORT_SIDECAR_NAME.to_string(),
            image: Some(data_mover_image()),
            command: Some(vec!["/bin/kaniop-data-mover".to_string()]),
            args: Some(vec![
                "transport".to_string(),
                "--operation-doc".to_string(),
                config.operation_doc_json.clone(),
            ]),
            env: Some(env_vars),
            volume_mounts: Some(volume_mounts),
            security_context: Some(hardened_security_context()),
            resources: Some(default_resource_requirements()),
            // Native sidecar semantics: restart independently without participating in Pod
            // readiness. Deliberately do not configure a readiness probe on this container.
            restart_policy: Some("Always".to_string()),
            ..Container::default()
        })
    }
}

fn replication_type(
    source_role: KanidmServerRole,
    target_role: KanidmServerRole,
) -> Option<ReplicationType> {
    match (source_role, target_role) {
        (
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUi,
            KanidmServerRole::WriteReplicaNoUi | KanidmServerRole::WriteReplica,
        ) => Some(ReplicationType::MutualPull),

        (
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUi,
            KanidmServerRole::ReadOnlyReplica,
        ) => Some(ReplicationType::AllowPull),
        (
            KanidmServerRole::ReadOnlyReplica,
            KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUi,
        ) => Some(ReplicationType::Pull),
        (KanidmServerRole::ReadOnlyReplica, KanidmServerRole::ReadOnlyReplica) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StatefulSetApplyStrategy, StatefulSetExt, TLS_SECRET_HASH_ANNOTATION,
        classify_statefulset_change, preserve_defaulted_statefulset_fields,
    };
    use crate::kanidm::crd::{
        Kanidm, KanidmSpec, KanidmStorage, PersistentVolumeClaimTemplate, ReplicaGroup,
    };
    use k8s_openapi::api::apps::v1::{
        StatefulSet, StatefulSetPersistentVolumeClaimRetentionPolicy,
    };
    use k8s_openapi::api::core::v1::{
        EmptyDirVolumeSource, EphemeralVolumeSource, PersistentVolumeClaim,
        PersistentVolumeClaimSpec, Volume,
    };
    use kube::api::ObjectMeta;

    fn create_kanidm_with_storage(storage: Option<KanidmStorage>) -> Kanidm {
        Kanidm {
            spec: KanidmSpec {
                storage,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub(super) fn create_kanidm_with_replica_group() -> (Kanidm, ReplicaGroup) {
        let replica_group = ReplicaGroup {
            name: "default".to_string(),
            replicas: 1,
            ..Default::default()
        };
        let kanidm = Kanidm {
            metadata: ObjectMeta {
                name: Some("test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmSpec {
                domain: "idm.example.com".to_string(),
                replica_groups: vec![replica_group.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        (kanidm, replica_group)
    }

    #[test]
    fn test_create_statefulset_with_tls_secret_hash_annotation() {
        let (kanidm, replica_group) = create_kanidm_with_replica_group();
        let sts = kanidm
            .create_statefulset(&replica_group, Some("abc123"), None)
            .unwrap();

        let annotations = sts
            .spec
            .unwrap()
            .template
            .metadata
            .unwrap()
            .annotations
            .unwrap();
        assert_eq!(
            annotations.get(TLS_SECRET_HASH_ANNOTATION),
            Some(&"abc123".to_string())
        );
    }

    #[test]
    fn test_create_statefulset_without_tls_secret_hash_annotation() {
        let (kanidm, replica_group) = create_kanidm_with_replica_group();
        let sts = kanidm
            .create_statefulset(&replica_group, None, None)
            .unwrap();

        let annotations = sts.spec.unwrap().template.metadata.unwrap().annotations;
        assert!(annotations.is_none());
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

    fn generated_statefulset() -> StatefulSet {
        let (kanidm, replica_group) = create_kanidm_with_replica_group();
        kanidm
            .create_statefulset(&replica_group, None, None)
            .unwrap()
    }

    #[test]
    fn statefulset_mutable_change_uses_apply() {
        let current = generated_statefulset();
        let mut desired = current.clone();
        desired.spec.as_mut().unwrap().replicas = Some(2);

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
    }

    #[test]
    fn statefulset_selector_change_requires_recreation() {
        let current = generated_statefulset();
        let mut desired = current.clone();
        desired
            .spec
            .as_mut()
            .unwrap()
            .selector
            .match_labels
            .get_or_insert_default()
            .insert("immutable".to_string(), "changed".to_string());

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Recreate {
                immutable_fields: vec!["spec.selector"]
            }
        );
    }

    #[test]
    fn statefulset_service_name_change_requires_recreation() {
        let current = generated_statefulset();
        let mut desired = current.clone();
        desired.spec.as_mut().unwrap().service_name = Some("different".to_string());

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Recreate {
                immutable_fields: vec!["spec.serviceName"]
            }
        );
    }

    #[test]
    fn statefulset_pod_management_default_does_not_recreate() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().pod_management_policy = Some("OrderedReady".to_string());
        let mut desired = generated_statefulset();
        preserve_defaulted_statefulset_fields(&mut desired, &current);

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
    }

    #[test]
    fn statefulset_non_default_pod_management_policy_requires_recreation() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().pod_management_policy = Some("Parallel".to_string());
        let mut desired = generated_statefulset();
        preserve_defaulted_statefulset_fields(&mut desired, &current);

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Recreate {
                immutable_fields: vec!["spec.podManagementPolicy"]
            }
        );
    }

    #[test]
    fn statefulset_volume_claim_template_change_is_non_destructive() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: Some("old".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        let mut desired = current.clone();
        desired
            .spec
            .as_mut()
            .unwrap()
            .volume_claim_templates
            .as_mut()
            .unwrap()[0]
            .spec
            .as_mut()
            .unwrap()
            .storage_class_name = Some("new".to_string());

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
    }

    #[test]
    fn statefulset_storage_class_defaulting_preserves_live_value() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: Some("fast".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        let mut desired = current.clone();
        desired
            .spec
            .as_mut()
            .unwrap()
            .volume_claim_templates
            .as_mut()
            .unwrap()[0]
            .spec
            .as_mut()
            .unwrap()
            .storage_class_name = None;
        preserve_defaulted_statefulset_fields(&mut desired, &current);

        // The CR cannot distinguish omission from removal after creation,
        // so compatibility/defaulting wins: live storageClassName is preserved.
        assert_eq!(
            desired
                .spec
                .as_ref()
                .unwrap()
                .volume_claim_templates
                .as_ref()
                .unwrap()[0]
                .spec
                .as_ref()
                .unwrap()
                .storage_class_name,
            Some("fast".to_string())
        );
        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
    }

    #[test]
    fn statefulset_server_defaults_do_not_require_recreation() {
        let mut desired = generated_statefulset();
        desired.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec::default()),
            ..Default::default()
        }]);
        let mut current = desired.clone();
        let current_spec = current.spec.as_mut().unwrap();
        current_spec.pod_management_policy = Some("OrderedReady".to_string());
        current_spec.volume_claim_templates.as_mut().unwrap()[0]
            .spec
            .as_mut()
            .unwrap()
            .volume_mode = Some("Filesystem".to_string());

        preserve_defaulted_statefulset_fields(&mut desired, &current);

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
    }

    #[test]
    fn statefulset_delete_retention_blocks_automatic_recreation() {
        let mut current = generated_statefulset();
        current
            .spec
            .as_mut()
            .unwrap()
            .persistent_volume_claim_retention_policy =
            Some(StatefulSetPersistentVolumeClaimRetentionPolicy {
                when_deleted: Some("Delete".to_string()),
                when_scaled: None,
            });
        let mut desired = current.clone();
        desired
            .spec
            .as_mut()
            .unwrap()
            .selector
            .match_labels
            .get_or_insert_default()
            .insert("immutable".to_string(), "changed".to_string());

        assert_eq!(
            classify_statefulset_change(&current, &desired),
            StatefulSetApplyStrategy::Apply
        );
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
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some())
        );
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
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some())
        );
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_emptydir_ephemeral_and_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ephemeral: Some(EphemeralVolumeSource::default()),
            volume_claim_template: Some(PersistentVolumeClaimTemplate::default()),
        });
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.len(), 1);
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some())
        );
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
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.ephemeral.is_some())
        );
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_ephemeral_and_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            ephemeral: Some(EphemeralVolumeSource::default()),
            volume_claim_template: Some(PersistentVolumeClaimTemplate::default()),
            ..Default::default()
        });
        let kanidm = create_kanidm_with_storage(storage);
        let (volumes, volume_claim_template) = kanidm.expand_storage(vec![]);

        assert_eq!(volumes.len(), 1);
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.ephemeral.is_some())
        );
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn test_generate_volumes_with_volumeclaimtemplate() {
        let storage = Some(KanidmStorage {
            volume_claim_template: Some(PersistentVolumeClaimTemplate::default()),
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
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some())
        );
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
        assert!(
            volumes
                .clone()
                .iter()
                .any(|v| v.name == "existing-volume-1")
        );
        assert!(
            volumes
                .clone()
                .iter()
                .any(|v| v.name == "existing-volume-2")
        );
        assert!(
            volumes
                .iter()
                .any(|v| v.name == "kanidm-data" && v.empty_dir.is_some())
        );
        assert!(volume_claim_template.is_none());
    }

    #[test]
    fn statefulset_storage_class_omission_preserves_live() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: Some("standard".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        let mut desired = generated_statefulset();
        desired.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: None,
                ..Default::default()
            }),
            ..Default::default()
        }]);
        preserve_defaulted_statefulset_fields(&mut desired, &current);

        // Omission in desired should preserve the live storageClassName
        assert_eq!(
            desired
                .spec
                .as_ref()
                .unwrap()
                .volume_claim_templates
                .as_ref()
                .unwrap()[0]
                .spec
                .as_ref()
                .unwrap()
                .storage_class_name,
            Some("standard".to_string())
        );
    }

    #[test]
    fn statefulset_storage_class_explicit_non_null_desired_is_preserved() {
        let mut current = generated_statefulset();
        current.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: Some("old".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        let mut desired = generated_statefulset();
        desired.spec.as_mut().unwrap().volume_claim_templates = Some(vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("kanidm-data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                storage_class_name: Some("new".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        // Only defaulting, not live-preservation (desired is non-None)
        preserve_defaulted_statefulset_fields(&mut desired, &current);

        // Explicit non-None desired value wins
        assert_eq!(
            desired
                .spec
                .as_ref()
                .unwrap()
                .volume_claim_templates
                .as_ref()
                .unwrap()[0]
                .spec
                .as_ref()
                .unwrap()
                .storage_class_name,
            Some("new".to_string())
        );
    }
}

#[cfg(all(test, feature = "integration-test"))]
mod integration_test {
    use super::{REPLICATION_CONFIG_IMAGE, REPLICATION_CONFIG_SCRIPT};

    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;
    use testcontainers::ContainerRequest;
    use testcontainers::GenericImage;
    use testcontainers::ImageExt;
    use testcontainers::core::Mount;
    use testcontainers::runners::AsyncRunner;
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
            .with_user(nix::unistd::getuid().to_string())
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
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-0.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
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
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-0.kanidm-test:8444"
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
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    (
                        "EXTERNAL_REPLICATION_NODE_HOST_0",
                        "repl://external-host-0:8444",
                    ),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_TYPE", "mutual-pull"),
                    ("EXTERNAL_REPLICATION_NODE_HOST_0_AUTOMATIC_REFRESH", "true"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-0.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-0.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-1.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-3"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-1"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-1.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-3.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-3"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-default-3"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-default-3.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1.kanidm-test:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("REPLICA_GROUP", "read-replica"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-read-replica-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-read-replica-0.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("REPLICA_GROUP", "read-replica"),
                    ("KANIDM_PRIMARY_NODE", "kanidm-test-default-0"),
                    ("POD_NAME", "kanidm-test-read-replica-1"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-read-replica-1.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://kanidm-test-default-0.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
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
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_0_HOST",
                        "kanidm-test-default-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_1_HOST",
                        "kanidm-test-default-1.kanidm-test",
                    ),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "pull"),
                    (
                        "KANIDM_TEST_DEFAULT_3_HOST",
                        "kanidm-test-default-3.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", ""),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://kanidm-test-read-replica-1.kanidm-test:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://external-host-0:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-external-host-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-0.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-0"
automatic_refresh = true

[replication."repl://kanidm-test-default-1.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://kanidm-test-default-3.kanidm-test:8444"]
type = "pull"
supplier_cert = "dummy-cert-default-3"
automatic_refresh = false

"#,
            },
            TestCase {
                env_vars: vec![
                    ("KANIDM_CONFIG_PATH", "/tmp/server.toml"),
                    ("REPLICATION_PORT", "8444"),
                    ("BIND_ADDRESS", "0.0.0.0"),
                    ("KANIDM_NAME", "kanidm-test"),
                    ("POD_NAME", "kanidm-test-default-0"),
                    ("KANIOP_REPLICATION_ENABLED", "true"),
                    ("KANIDM_TEST_DEFAULT_0", "dummy-cert-default-0"),
                    ("KANIDM_TEST_DEFAULT_0_HOST", "10.200.20.1"),
                    ("KANIDM_TEST_DEFAULT_0_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_1", "dummy-cert-default-1"),
                    ("KANIDM_TEST_DEFAULT_1_HOST", "10.200.20.2"),
                    ("KANIDM_TEST_DEFAULT_1_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_DEFAULT_3", "dummy-cert-default-3"),
                    ("KANIDM_TEST_DEFAULT_3_HOST", "10.200.20.4"),
                    ("KANIDM_TEST_DEFAULT_3_TYPE", "mutual-pull"),
                    ("KANIDM_TEST_READ_REPLICA_0", "dummy-cert-read-replica-0"),
                    ("KANIDM_TEST_READ_REPLICA_0_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_0_HOST",
                        "kanidm-test-read-replica-0.kanidm-test",
                    ),
                    ("KANIDM_TEST_READ_REPLICA_1", "dummy-cert-read-replica-1"),
                    ("KANIDM_TEST_READ_REPLICA_1_TYPE", "allow-pull"),
                    (
                        "KANIDM_TEST_READ_REPLICA_1_HOST",
                        "kanidm-test-read-replica-1.kanidm-test",
                    ),
                ],
                expected_result: r#"version = "2"

[replication]
origin = "repl://10.200.20.1:8444"
bindaddress = "0.0.0.0:8444"

[replication."repl://10.200.20.2:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-1"
automatic_refresh = false

[replication."repl://10.200.20.4:8444"]
type = "mutual-pull"
partner_cert = "dummy-cert-default-3"
automatic_refresh = false

[replication."repl://kanidm-test-read-replica-0.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-0"

[replication."repl://kanidm-test-read-replica-1.kanidm-test:8444"]
type = "allow-pull"
consumer_cert = "dummy-cert-read-replica-1"

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

    #[test]
    fn backup_enables_generated_config_and_native_online_backup_stanza() {
        use crate::kanidm::crd::{KanidmStorage, PersistentVolumeClaimTemplate};
        use k8s_openapi::api::core::v1::PersistentVolumeClaimSpec;

        use crate::kanidm::reconcile::transport::BackupConfig;

        use super::StatefulSetExt;

        let (mut kanidm, mut replica_group) = super::tests::create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];
        let backup_config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: None,
        };
        kanidm.spec.storage = Some(KanidmStorage {
            volume_claim_template: Some(PersistentVolumeClaimTemplate {
                metadata: None,
                spec: Some(PersistentVolumeClaimSpec::default()),
            }),
            ..Default::default()
        });

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&backup_config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();
        let init = pod
            .init_containers
            .unwrap()
            .into_iter()
            .find(|c| c.name == "kanidm-generate-replication-config")
            .unwrap();
        let env = init.env.unwrap();
        assert!(
            env.iter()
                .any(|e| e.name == "KANIOP_BACKUP_ENABLED" && e.value.as_deref() == Some("true"))
        );
        assert!(
            env.iter()
                .any(|e| e.name == "KANIOP_BACKUP_SCHEDULE"
                    && e.value.as_deref() == Some("0 2 * * *"))
        );
        assert!(
            env.iter()
                .any(|e| e.name == "KANIOP_BACKUP_VERSIONS" && e.value.as_deref() == Some("7"))
        );
        let script = init.args.unwrap().join("\n");
        assert!(script.contains("[online_backup]"));
        assert!(script.contains("env.POD_NAME == primary_node"));
    }

    #[test]
    fn transport_sidecar_is_native_sidecar_for_primary_group_with_config() {
        use super::StatefulSetExt;
        use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};
        use kaniop_backup_core::crd::{AuthMethod, SecretRef};

        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: Some(TransportSidecarConfig {
                operation_doc_json: r#"{"operation":"transport","bucket":"test"}"#.to_string(),
                auth_method: AuthMethod {
                    workload_identity: None,
                    secret_ref: Some(SecretRef {
                        name: "writer-secret".to_string(),
                    }),
                },
                ca_bundle_ref: None,
                encryption_key_ref: None,
            }),
        };

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();
        assert!(
            pod.containers
                .iter()
                .all(|c| c.name != "data-mover-transport")
        );
        let sidecar = pod
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.name == "data-mover-transport")
            .expect("transport native sidecar should be present");
        assert_eq!(sidecar.restart_policy.as_deref(), Some("Always"));
        assert!(sidecar.readiness_probe.is_none());
        assert_eq!(
            sidecar.image.as_deref(),
            Some("ghcr.io/pando85/kaniop-data-mover:latest")
        );
        assert_eq!(
            sidecar.command.as_ref().unwrap(),
            &vec!["/bin/kaniop-data-mover".to_string()]
        );
        assert_eq!(
            sidecar.args.as_ref().unwrap(),
            &vec![
                "transport".to_string(),
                "--operation-doc".to_string(),
                r#"{"operation":"transport","bucket":"test"}"#.to_string(),
            ]
        );

        let env = sidecar.env.as_ref().unwrap();
        assert!(env.iter().any(|e| e.name == "POD_NAME"));
        assert!(env.iter().any(|e| e.name == "KANIDM_PRIMARY_NODE"));
        assert!(env.iter().any(|e| e.name == "AWS_ACCESS_KEY_ID"));

        let mounts = sidecar.volume_mounts.as_ref().unwrap();
        assert!(
            mounts
                .iter()
                .any(|m| m.name == "kanidm-data" && m.read_only == Some(true))
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.name == "kanidm-tmp" && m.mount_path == "/tmp")
        );

        let volumes = pod.volumes.as_ref().unwrap();
        assert!(volumes.iter().any(|v| v.name == "kanidm-tmp"));

        let sc = sidecar.security_context.as_ref().unwrap();
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert!(sc.run_as_user.is_none());
        assert!(
            sc.capabilities
                .as_ref()
                .unwrap()
                .drop
                .as_ref()
                .unwrap()
                .contains(&"ALL".to_string())
        );
    }

    #[test]
    fn transport_sidecar_absent_for_non_primary_group() {
        use super::StatefulSetExt;
        use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};
        use kaniop_backup_core::crd::{AuthMethod, SecretRef};

        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = false;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: Some(TransportSidecarConfig {
                operation_doc_json: r#"{"operation":"transport"}"#.to_string(),
                auth_method: AuthMethod {
                    workload_identity: None,
                    secret_ref: Some(SecretRef {
                        name: "writer-secret".to_string(),
                    }),
                },
                ca_bundle_ref: None,
                encryption_key_ref: None,
            }),
        };

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();
        assert!(
            pod.containers
                .iter()
                .all(|c| c.name != "data-mover-transport")
        );
        assert!(
            pod.init_containers
                .as_ref()
                .is_none_or(|containers| containers
                    .iter()
                    .all(|c| c.name != "data-mover-transport"))
        );
    }

    #[test]
    fn transport_sidecar_absent_without_config() {
        use super::StatefulSetExt;
        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let sts = kanidm
            .create_statefulset(&replica_group, None, None)
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();
        assert!(
            pod.containers
                .iter()
                .all(|c| c.name != "data-mover-transport")
        );
        assert!(
            pod.init_containers
                .as_ref()
                .is_none_or(|containers| containers
                    .iter()
                    .all(|c| c.name != "data-mover-transport"))
        );
    }

    #[test]
    fn transport_sidecar_with_ca_bundle() {
        use super::StatefulSetExt;
        use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};
        use kaniop_backup_core::crd::{AuthMethod, SecretRef};

        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: Some(TransportSidecarConfig {
                operation_doc_json: r#"{"operation":"transport"}"#.to_string(),
                auth_method: AuthMethod {
                    workload_identity: None,
                    secret_ref: Some(SecretRef {
                        name: "writer-secret".to_string(),
                    }),
                },
                ca_bundle_ref: Some("ca-config-map".to_string()),
                encryption_key_ref: None,
            }),
        };

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();

        let sidecar = pod
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.name == "data-mover-transport")
            .unwrap();
        let env = sidecar.env.as_ref().unwrap();
        assert!(env.iter().any(|e| e.name == "SSL_CERT_FILE"));

        let mounts = sidecar.volume_mounts.as_ref().unwrap();
        assert!(mounts.iter().any(|m| m.name == "ca-bundle"));

        let volumes = pod.volumes.as_ref().unwrap();
        assert!(volumes.iter().any(|v| v.name == "ca-bundle"));
    }

    #[test]
    fn transport_sidecar_includes_encryption_key_env_when_key_ref_set() {
        use super::StatefulSetExt;
        use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};
        use kaniop_backup_core::crd::{AuthMethod, SecretRef};

        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: Some(TransportSidecarConfig {
                operation_doc_json: r#"{"operation":"transport"}"#.to_string(),
                auth_method: AuthMethod {
                    workload_identity: None,
                    secret_ref: Some(SecretRef {
                        name: "writer-secret".to_string(),
                    }),
                },
                ca_bundle_ref: None,
                encryption_key_ref: Some(SecretRef {
                    name: "kek-secret".to_string(),
                }),
            }),
        };

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();

        let sidecar = pod
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.name == "data-mover-transport")
            .unwrap();
        let env = sidecar.env.as_ref().unwrap();
        let enc_env = env.iter().find(|e| e.name == "KANIOP_ENCRYPTION_KEY");
        assert!(
            enc_env.is_some(),
            "KANIOP_ENCRYPTION_KEY env var must be present"
        );
        let enc_env = enc_env.unwrap();
        let secret_ref = enc_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.name, "kek-secret");
        assert_eq!(secret_ref.key, "encryption-key");
    }

    #[test]
    fn transport_sidecar_omits_encryption_key_env_when_no_key_ref() {
        use super::StatefulSetExt;
        use crate::kanidm::reconcile::transport::{BackupConfig, TransportSidecarConfig};
        use kaniop_backup_core::crd::{AuthMethod, SecretRef};

        use super::tests::create_kanidm_with_replica_group;
        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];

        let config = BackupConfig {
            schedule: "0 2 * * *".to_string(),
            local_versions: 7,
            transport: Some(TransportSidecarConfig {
                operation_doc_json: r#"{"operation":"transport"}"#.to_string(),
                auth_method: AuthMethod {
                    workload_identity: None,
                    secret_ref: Some(SecretRef {
                        name: "writer-secret".to_string(),
                    }),
                },
                ca_bundle_ref: None,
                encryption_key_ref: None,
            }),
        };

        let sts = kanidm
            .create_statefulset(&replica_group, None, Some(&config))
            .unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();

        let sidecar = pod
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.name == "data-mover-transport")
            .unwrap();
        let env = sidecar.env.as_ref().unwrap();
        assert!(
            !env.iter().any(|e| e.name == "KANIOP_ENCRYPTION_KEY"),
            "KANIOP_ENCRYPTION_KEY must be absent when key_ref is None"
        );
    }
}
