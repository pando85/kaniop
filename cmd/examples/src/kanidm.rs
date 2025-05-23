use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        apps::v1::StatefulSetPersistentVolumeClaimRetentionPolicy,
        core::v1::{
            Affinity, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec, PodAffinityTerm,
            PodAntiAffinity, ResourceRequirements, SecretKeySelector, Toleration,
            TopologySpreadConstraint, VolumeResourceRequirements,
        },
    },
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::LabelSelector},
};
use kaniop_operator::kanidm::{
    crd::{
        ExternalReplicationNode, Kanidm, KanidmIngress, KanidmLogLevel, KanidmServerRole,
        KanidmService, KanidmSpec, KanidmStorage, ReplicaGroup, ReplicationType,
    },
    reconcile::{CLUSTER_LABEL, statefulset::REPLICA_GROUP_LABEL},
};

use kube::api::ObjectMeta;
use schemars::{r#gen::SchemaGenerator, schema::RootSchema};

pub fn example() -> Kanidm {
    let name = "my-idm";
    let replica_group_name = "default";
    Kanidm {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: KanidmSpec {
            domain: format!("{name}.localhost"),
            replica_groups: vec![ReplicaGroup {
                name: replica_group_name.to_string(),
                replicas: 1,
                role: KanidmServerRole::WriteReplica,
                primary_node: true,
                resources: Some(ResourceRequirements {
                    requests: Some(BTreeMap::from([
                        ("cpu".to_string(), Quantity("100m".to_string())),
                        ("memory".to_string(), Quantity("64Mi".to_string())),
                    ])),
                    limits: Some(BTreeMap::from([
                        ("cpu".to_string(), Quantity("1".to_string())),
                        ("memory".to_string(), Quantity("128Mi".to_string())),
                    ])),
                    ..Default::default()
                }),
                node_selector: Some(BTreeMap::from([(
                    "kubernetes.io/arch".to_string(),
                    "arm64".to_string(),
                )])),
                affinity: Some(Affinity {
                    node_affinity: Some(Default::default()),
                    pod_affinity: Some(Default::default()),
                    pod_anti_affinity: Some(PodAntiAffinity {
                        preferred_during_scheduling_ignored_during_execution: Default::default(),
                        required_during_scheduling_ignored_during_execution: Some(vec![
                            PodAffinityTerm {
                                label_selector: Some(LabelSelector {
                                    match_labels: Some(BTreeMap::from([
                                        (CLUSTER_LABEL.to_string(), name.to_string()),
                                        (
                                            REPLICA_GROUP_LABEL.to_string(),
                                            replica_group_name.to_string(),
                                        ),
                                    ])),
                                    ..Default::default()
                                }),
                                topology_key: "kubernetes.io/hostname".to_string(),
                                ..Default::default()
                            },
                        ]),
                    }),
                }),
                tolerations: Some(vec![Toleration {
                    key: Some("dedicated".to_string()),
                    operator: Some("Equal".to_string()),
                    value: Some("kanidm".to_string()),
                    effect: Some("NoSchedule".to_string()),
                    ..Default::default()
                }]),
                topology_spread_constraints: Some(vec![TopologySpreadConstraint {
                    max_skew: 1,
                    topology_key: "kubernetes.io/hostname".to_string(),
                    when_unsatisfiable: "DoNotSchedule".to_string(),
                    label_selector: Some(LabelSelector {
                        match_labels: Some(BTreeMap::from([
                            (CLUSTER_LABEL.to_string(), name.to_string()),
                            (
                                REPLICA_GROUP_LABEL.to_string(),
                                replica_group_name.to_string(),
                            ),
                        ])),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            }],
            external_replication_nodes: vec![ExternalReplicationNode {
                name: "my-idm-external".to_string(),
                hostname: "my-idm-external.localhost".to_string(),
                port: 8444,
                certificate: SecretKeySelector {
                    name: "my-idm-external-certificate".to_string(),
                    key: "tls.crt".to_string(),
                    optional: Some(false),
                },
                _type: ReplicationType::MutualPull,
                automatic_refresh: true,
            }],
            image: "kanidm/server:latest".to_string(),
            log_level: KanidmLogLevel::Info,
            port_name: "https".to_string(),
            image_pull_policy: Some("Always".to_string()),
            env: Some(vec![EnvVar {
                name: "KANIDM_DB_ARC_SIZE".to_string(),
                value: Some("2048".to_string()),
                ..Default::default()
            }]),
            oauth2_client_namespace_selector: Some(Default::default()),
            storage: Some(KanidmStorage {
                empty_dir: Some(Default::default()),
                ephemeral: Some(Default::default()),
                volume_claim_template: Some(PersistentVolumeClaim {
                    metadata: Default::default(),
                    spec: Some(PersistentVolumeClaimSpec {
                        access_modes: Some(vec!["ReadWriteOnce".to_string()]),
                        resources: Some(VolumeResourceRequirements {
                            requests: Some(BTreeMap::from([(
                                "storage".to_string(),
                                Quantity("100Mi".to_string()),
                            )])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    status: None,
                }),
            }),
            ldap_port_name: Some("ldap".to_string()),
            tls_secret_name: Some("my-idm-tls".to_string()),
            service: Some(KanidmService {
                annotations: Some(BTreeMap::from([(
                    "service.beta.kubernetes.io/aws-load-balancer-backend-protocol".to_string(),
                    "tcp".to_string(),
                )])),
                type_: Some("ClusterIP".to_string()),
            }),
            ingress: Some(KanidmIngress {
                annotations: Some(BTreeMap::from([(
                    "nginx.ingress.kubernetes.io/backend-protocol".to_string(),
                    "HTTPS".to_string(),
                )])),
                ingress_class_name: Some("nginx".to_string()),
                tls_secret_name: Some("my-idm-tls".to_string()),
            }),
            volumes: Some(vec![]),
            volume_mounts: Some(vec![]),
            persistent_volume_claim_retention_policy: Some(
                StatefulSetPersistentVolumeClaimRetentionPolicy {
                    ..Default::default()
                },
            ),
            security_context: Some(Default::default()),
            dns_config: Some(Default::default()),
            dns_policy: Some(Default::default()),
            containers: Some(vec![]),
            init_containers: Some(vec![]),
            min_ready_seconds: Some(0),
            host_aliases: Some(vec![]),
            host_network: Some(false),
        },
        status: Default::default(),
    }
}

pub fn schema(generator: &SchemaGenerator) -> RootSchema {
    generator.clone().into_root_schema_for::<Kanidm>()
}
