//! Kubernetes workload builders for an independent Kanidm RADIUS endpoint.
//!
//! Pure construction only: reconciliation must verify parent UID ownership,
//! validate referenced inputs, and preserve last-good credentials before apply.
//! This does not mint Kanidm tokens or adopt existing Kubernetes objects.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{
    Deployment, DeploymentSpec, DeploymentStrategy, RollingUpdateDeployment,
};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, ConfigMapProjection, Container, ContainerPort, ExecAction, KeyToPath,
    PodSecurityContext, PodSpec, PodTemplateSpec, Probe, ProjectedVolumeSource,
    ResourceRequirements, SeccompProfile, SecretProjection, SecurityContext, Service, ServicePort,
    ServiceSpec, Toleration, TopologySpreadConstraint, Volume, VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use thiserror::Error;
use kube::{Resource, ResourceExt};

use super::radius_crd::{KanidmRadiusServer, RadiusCaKind};
use super::radius_rotation::TokenGeneration;

const REVISION_ANNOTATION: &str = "kaniop.rs/radius-revision";
const COMPONENT: &str = "kanidm-radius";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadiusCaSource {
    Secret { name: String, key: String },
    ConfigMap { name: String, key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadiusServiceExposure {
    ClusterIP,
    NodePort {
        external_traffic_policy: Option<String>,
    },
    LoadBalancer {
        external_traffic_policy: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct RadiusWorkload {
    pub name: String,
    pub namespace: String,
    /// The owner must be the RADIUS CR itself, not the referenced Kanidm CR.
    pub owner: OwnerReference,
    pub image: String,
    pub replicas: i32,
    pub configuration_secret: String,
    /// The revision is an opaque, non-secret identifier, never a credential hash.
    pub revision: String,
    pub eap_tls_secret: String,
    pub eap_certificate_key: String,
    pub eap_private_key_key: String,
    pub eap_ca_key: Option<String>,
    pub kanidm_ca: Option<RadiusCaSource>,
    pub exposure: RadiusServiceExposure,
    pub service_port: i32,
    pub service_annotations: BTreeMap<String, String>,
    pub resources: Option<ResourceRequirements>,
    pub node_selector: Option<BTreeMap<String, String>>,
    pub affinity: Option<Affinity>,
    pub tolerations: Option<Vec<Toleration>>,
    pub topology_spread_constraints: Option<Vec<TopologySpreadConstraint>>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadiusWorkloadError {
    #[error("RADIUS resource name, namespace, owner UID, or image is missing")]
    MissingResourceIdentity,
    #[error("RADIUS workload requires at least one replica")]
    InvalidReplicas,
    #[error("RADIUS configuration and certificate Secret references must not be empty")]
    MissingSecretReference,
    #[error("RADIUS Service port must be a valid TCP/UDP port")]
    InvalidPort,
    #[error("RADIUS externalTrafficPolicy must be Cluster or Local")]
    InvalidExternalTrafficPolicy,
}

/// Derive the entire desired workload from the CR and an immutable token
/// generation. Never derive owner identity from the reusable resource name.
pub fn workload_for_generation(
    radius: &KanidmRadiusServer,
    image: String,
    generation: &TokenGeneration,
) -> Result<RadiusWorkload, RadiusWorkloadError> {
    let namespace = radius
        .namespace()
        .ok_or(RadiusWorkloadError::MissingResourceIdentity)?;
    let owner = radius
        .controller_owner_ref(&())
        .ok_or(RadiusWorkloadError::MissingResourceIdentity)?;
    let kanidm_ca = radius
        .spec
        .kanidm_tls
        .as_ref()
        .and_then(|tls| tls.ca_bundle_ref.as_ref())
        .map(|ca| match ca.kind {
            RadiusCaKind::ConfigMap => RadiusCaSource::ConfigMap {
                name: ca.name.clone(),
                key: ca.key.clone(),
            },
            RadiusCaKind::Secret => RadiusCaSource::Secret {
                name: ca.name.clone(),
                key: ca.key.clone(),
            },
        });
    let policy = radius.spec.service.external_traffic_policy.clone();
    let exposure = match radius.spec.service.type_.as_str() {
        "ClusterIP" => RadiusServiceExposure::ClusterIP,
        "NodePort" => RadiusServiceExposure::NodePort {
            external_traffic_policy: policy,
        },
        "LoadBalancer" => RadiusServiceExposure::LoadBalancer {
            external_traffic_policy: policy,
        },
        _ => return Err(RadiusWorkloadError::InvalidExternalTrafficPolicy),
    };
    let tls = &radius.spec.radius_tls;
    let workload = RadiusWorkload {
        name: radius.name_any(),
        namespace,
        owner,
        image,
        replicas: radius.spec.replicas,
        configuration_secret: generation.config_secret.clone(),
        revision: generation.revision.clone(),
        eap_tls_secret: tls.secret_ref.name.clone(),
        eap_certificate_key: tls.cert_key.clone(),
        eap_private_key_key: tls.private_key_key.clone(),
        eap_ca_key: tls.ca_key.clone(),
        kanidm_ca,
        exposure,
        service_port: radius.spec.service.port,
        service_annotations: radius.spec.service.annotations.clone(),
        resources: radius.spec.resources.clone(),
        node_selector: radius.spec.node_selector.clone(),
        affinity: radius.spec.affinity.clone(),
        tolerations: radius.spec.tolerations.clone(),
        topology_spread_constraints: radius.spec.topology_spread_constraints.clone(),
    };
    workload.validate()?;
    Ok(workload)
}

impl RadiusWorkload {
    pub fn validate(&self) -> Result<(), RadiusWorkloadError> {
        if self.name.is_empty()
            || self.namespace.is_empty()
            || self.owner.uid.is_empty()
            || self.image.trim().is_empty()
            || self.revision.is_empty()
        {
            return Err(RadiusWorkloadError::MissingResourceIdentity);
        }
        if self.replicas < 1 {
            return Err(RadiusWorkloadError::InvalidReplicas);
        }
        if self.configuration_secret.is_empty()
            || self.eap_tls_secret.is_empty()
            || self.eap_certificate_key.is_empty()
            || self.eap_private_key_key.is_empty()
        {
            return Err(RadiusWorkloadError::MissingSecretReference);
        }
        if !(1..=65535).contains(&self.service_port) {
            return Err(RadiusWorkloadError::InvalidPort);
        }
        let policy = match &self.exposure {
            RadiusServiceExposure::ClusterIP => None,
            RadiusServiceExposure::NodePort {
                external_traffic_policy,
            }
            | RadiusServiceExposure::LoadBalancer {
                external_traffic_policy,
            } => external_traffic_policy.as_deref(),
        };
        if let Some(policy) = policy {
            if !matches!(policy, "Cluster" | "Local") {
                return Err(RadiusWorkloadError::InvalidExternalTrafficPolicy);
            }
        }
        Ok(())
    }

    fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("app.kubernetes.io/name".into(), COMPONENT.into()),
            ("kaniop.rs/radius-uid".into(), self.owner.uid.clone()),
        ])
    }

    fn owner_metadata(&self) -> ObjectMeta {
        ObjectMeta {
            name: Some(self.name.clone()),
            namespace: Some(self.namespace.clone()),
            labels: Some(self.labels()),
            owner_references: Some(vec![self.owner.clone()]),
            ..Default::default()
        }
    }

    /// Project all files into the single /data mount expected by the official
    /// bootstrap. No subPath mounts: each input is a user-owned Secret/ConfigMap,
    /// and the rendered configuration uses a revisioned operator-owned Secret.
    fn projections(&self) -> Vec<VolumeProjection> {
        let mut projections = vec![
            VolumeProjection {
                secret: Some(SecretProjection {
                    name: self.configuration_secret.clone(),
                    items: Some(vec![KeyToPath {
                        key: "radius.toml".into(),
                        path: "radius.toml".into(),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            VolumeProjection {
                secret: Some(SecretProjection {
                    name: self.eap_tls_secret.clone(),
                    items: Some({
                        let mut items = vec![
                            KeyToPath {
                                key: self.eap_certificate_key.clone(),
                                path: "cert.pem".into(),
                                ..Default::default()
                            },
                            KeyToPath {
                                key: self.eap_private_key_key.clone(),
                                path: "key.pem".into(),
                                ..Default::default()
                            },
                        ];
                        if let Some(key) = &self.eap_ca_key {
                            items.push(KeyToPath {
                                key: key.clone(),
                                path: "ca.pem".into(),
                                ..Default::default()
                            });
                        }
                        items
                    }),
                    optional: Some(false),
                }),
                ..Default::default()
            },
        ];

        if let Some(ca) = &self.kanidm_ca {
            projections.push(match ca {
                RadiusCaSource::Secret { name, key } => VolumeProjection {
                    secret: Some(SecretProjection {
                        name: name.clone(),
                        items: Some(vec![KeyToPath {
                            key: key.clone(),
                            path: "kanidm-ca.pem".into(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                RadiusCaSource::ConfigMap { name, key } => VolumeProjection {
                    config_map: Some(ConfigMapProjection {
                        name: name.clone(),
                        items: Some(vec![KeyToPath {
                            key: key.clone(),
                            path: "kanidm-ca.pem".into(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            });
        }
        projections
    }

    pub fn deployment(&self) -> Result<Deployment, RadiusWorkloadError> {
        self.validate()?;
        let labels = self.labels();
        let container = Container {
            name: COMPONENT.into(),
            image: Some(self.image.clone()),
            resources: self.resources.clone(),
            // A static configuration check is better than a ready-but-dead
            // UDP process. It does not validate live Kanidm authentication:
            // token promotion/revocation requires a separate real login probe.
            readiness_probe: Some(Probe {
                exec: Some(ExecAction {
                    command: Some(vec!["/usr/sbin/radiusd".into(), "-C".into()]),
                }),
                period_seconds: Some(10),
                timeout_seconds: Some(5),
                failure_threshold: Some(3),
                ..Default::default()
            }),
            ports: Some(vec![ContainerPort {
                name: Some("radius-auth".into()),
                container_port: 1812,
                protocol: Some("UDP".into()),
                ..Default::default()
            }]),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                capabilities: Some(Capabilities {
                    drop: Some(vec!["ALL".into()]),
                    ..Default::default()
                }),
                // The published kanidm/radius:1.11.2 image resolves its
                // radiusd account to UID 497 and GID 496 (verified by CI).
                // Explicit numeric identity is necessary for Kubelet's
                // runAsNonRoot validation when the image USER is named.
                run_as_user: Some(497),
                run_as_group: Some(496),
                run_as_non_root: Some(true),
                ..Default::default()
            }),
            volume_mounts: Some(vec![VolumeMount {
                name: "radius-data".into(),
                mount_path: "/data".into(),
                read_only: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let pod = PodSpec {
            containers: vec![container],
            node_selector: self.node_selector.clone(),
            affinity: self.affinity.clone(),
            tolerations: self.tolerations.clone(),
            topology_spread_constraints: self.topology_spread_constraints.clone(),
            termination_grace_period_seconds: Some(30),
            automount_service_account_token: Some(false),
            enable_service_links: Some(false),
            security_context: Some(PodSecurityContext {
                run_as_user: Some(497),
                run_as_group: Some(496),
                run_as_non_root: Some(true),
                fs_group: Some(496),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            volumes: Some(vec![Volume {
                name: "radius-data".into(),
                projected: Some(ProjectedVolumeSource {
                    sources: Some(self.projections()),
                    // The non-root RADIUS process must be able to read its
                    // certificate and private key from the projected volume.
                    // The K8s Secret API and pod access must remain restricted.
                    default_mode: Some(0o440),
                }),
                ..Default::default()
            }]),
            ..Default::default()
        };
        Ok(Deployment {
            metadata: self.owner_metadata(),
            spec: Some(DeploymentSpec {
                replicas: Some(self.replicas),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels),
                        annotations: Some(BTreeMap::from([(
                            REVISION_ANNOTATION.to_string(),
                            self.revision.clone(),
                        )])),
                        ..Default::default()
                    }),
                    spec: Some(pod),
                },
                strategy: Some(DeploymentStrategy {
                    type_: Some("RollingUpdate".into()),
                    rolling_update: Some(RollingUpdateDeployment {
                        max_surge: Some(IntOrString::Int(1)),
                        max_unavailable: Some(IntOrString::Int(0)),
                    }),
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    pub fn service(&self) -> Result<Service, RadiusWorkloadError> {
        self.validate()?;
        let (service_type, external_traffic_policy) = match &self.exposure {
            RadiusServiceExposure::ClusterIP => ("ClusterIP", None),
            RadiusServiceExposure::NodePort {
                external_traffic_policy,
            } => ("NodePort", external_traffic_policy.clone()),
            RadiusServiceExposure::LoadBalancer {
                external_traffic_policy,
            } => ("LoadBalancer", external_traffic_policy.clone()),
        };
        let mut metadata = self.owner_metadata();
        if !self.service_annotations.is_empty() {
            metadata.annotations = Some(self.service_annotations.clone());
        }
        Ok(Service {
            metadata,
            spec: Some(ServiceSpec {
                type_: Some(service_type.into()),
                external_traffic_policy,
                selector: Some(self.labels()),
                ports: Some(vec![ServicePort {
                    name: Some("radius-auth".into()),
                    port: self.service_port,
                    protocol: Some("UDP".into()),
                    target_port: Some(IntOrString::Int(1812)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workload() -> RadiusWorkload {
        RadiusWorkload {
            name: "campus-radius".into(),
            namespace: "network-auth".into(),
            owner: OwnerReference {
                api_version: "kaniop.rs/v1beta1".into(),
                kind: "KanidmRadiusServer".into(),
                name: "campus-radius".into(),
                uid: "d10066c3-4599-4487-972d-26bb42587a32".into(),
                controller: Some(true),
                block_owner_deletion: Some(true),
            },
            image: "kanidm/radius:1.11.2".into(),
            replicas: 2,
            configuration_secret: "campus-radius-cfg-1".into(),
            revision: "1".into(),
            eap_tls_secret: "radius-tls".into(),
            eap_certificate_key: "tls.crt".into(),
            eap_private_key_key: "tls.key".into(),
            eap_ca_key: Some("ca.crt".into()),
            kanidm_ca: Some(RadiusCaSource::ConfigMap {
                name: "kanidm-ca".into(),
                key: "ca.crt".into(),
            }),
            exposure: RadiusServiceExposure::ClusterIP,
            service_port: 1812,
            service_annotations: BTreeMap::new(),
            resources: None,
            node_selector: None,
            affinity: None,
            tolerations: None,
            topology_spread_constraints: None,
        }
    }

    #[test]
    fn derives_workload_from_uid_owned_cr_and_secret_ca() {
        let radius: KanidmRadiusServer = serde_json::from_value(serde_json::json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmRadiusServer",
            "metadata": {
                "name": "campus-radius",
                "namespace": "network-auth",
                "uid": "d10066c3-4599-4487-972d-26bb42587a32"
            },
            "spec": {
                "kanidmRef": {"name": "idm"},
                "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
                "clients": [{
                    "name": "nas",
                    "ipaddr": "192.0.2.1",
                    "sharedSecretRef": {"name": "nas-secret", "key": "nas"}
                }],
                "radiusTls": {"secretRef": {"name": "eap-tls"}},
                "kanidmTls": {
                    "caBundleRef": {
                        "kind": "Secret",
                        "name": "private-ca",
                        "key": "ca.crt"
                    }
                },
                "service": {"type": "LoadBalancer", "externalTrafficPolicy": "Local"}
            }
        }))
        .unwrap();
        let generation = TokenGeneration {
            token_id: "token-id".into(),
            config_secret: "immutable-generation".into(),
            revision: "revision-1".into(),
        };
        let workload = workload_for_generation(
            &radius,
            "kanidm/radius:1.11.2".into(),
            &generation,
        )
        .unwrap();
        assert_eq!(workload.owner.uid, "d10066c3-4599-4487-972d-26bb42587a32");
        assert_eq!(workload.configuration_secret, "immutable-generation");
        assert_eq!(workload.revision, "revision-1");
        assert!(matches!(
            workload.kanidm_ca,
            Some(RadiusCaSource::Secret { ref name, ref key })
                if name == "private-ca" && key == "ca.crt"
        ));
        assert!(matches!(
            workload.exposure,
            RadiusServiceExposure::LoadBalancer {
                external_traffic_policy: Some(ref policy)
            } if policy == "Local"
        ));
    }

    #[test]
    fn deployment_has_no_kubernetes_token_or_injected_services() {
        let deployment = workload().deployment().unwrap();
        let spec = deployment.spec.unwrap();
        assert_eq!(spec.replicas, Some(2));
        assert_eq!(
            spec.strategy.unwrap().type_.as_deref(),
            Some("RollingUpdate")
        );
        let pod = spec.template.spec.unwrap();
        assert_eq!(pod.automount_service_account_token, Some(false));
        assert_eq!(pod.enable_service_links, Some(false));
        assert_eq!(pod.termination_grace_period_seconds, Some(30));
        assert_eq!(
            pod.containers[0]
                .readiness_probe
                .as_ref()
                .unwrap()
                .exec
                .as_ref()
                .unwrap()
                .command,
            Some(vec!["/usr/sbin/radiusd".into(), "-C".into()])
        );
        let security = pod.security_context.as_ref().unwrap();
        assert_eq!(security.run_as_user, Some(497));
        assert_eq!(security.run_as_non_root, Some(true));
        assert_eq!(security.fs_group, Some(496));
        assert_eq!(
            security.seccomp_profile.as_ref().unwrap().type_,
            "RuntimeDefault"
        );

        assert_eq!(
            pod.containers[0].image.as_deref(),
            Some("kanidm/radius:1.11.2")
        );
        assert_eq!(
            pod.containers[0].ports.as_ref().unwrap()[0]
                .protocol
                .as_deref(),
            Some("UDP")
        );
        assert_eq!(
            pod.containers[0].volume_mounts.as_ref().unwrap()[0].mount_path,
            "/data"
        );
        assert!(
            pod.containers[0].volume_mounts.as_ref().unwrap()[0]
                .read_only
                .unwrap()
        );
        assert_eq!(
            pod.volumes.unwrap()[0]
                .projected
                .as_ref()
                .unwrap()
                .sources
                .as_ref()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn revision_change_updates_pod_template_without_touching_client_secrets() {
        let a = workload().deployment().unwrap();
        let mut updated = workload();
        updated.revision = "2".into();
        updated.configuration_secret = "campus-radius-cfg-2".into();
        let b = updated.deployment().unwrap();
        assert_ne!(
            a.spec.as_ref().unwrap().template,
            b.spec.as_ref().unwrap().template
        );
        let deps = b.spec.unwrap().template.spec.unwrap().volumes.unwrap();
        let projected = deps[0]
            .projected
            .as_ref()
            .unwrap()
            .sources
            .as_ref()
            .unwrap();
        assert_eq!(
            projected[1].secret.as_ref().unwrap().name.as_str(),
            "radius-tls"
        );
    }

    #[test]
    fn service_is_udp_only_and_uses_owner_uid_selector() {
        let mut input = workload();
        input.exposure = RadiusServiceExposure::LoadBalancer {
            external_traffic_policy: Some("Local".into()),
        };
        let service = input.service().unwrap();
        assert_eq!(
            service.metadata.owner_references.as_ref().unwrap()[0].uid,
            input.owner.uid
        );
        let spec = service.spec.unwrap();
        assert_eq!(spec.type_.as_deref(), Some("LoadBalancer"));
        assert_eq!(spec.external_traffic_policy.as_deref(), Some("Local"));
        let port = &spec.ports.unwrap()[0];
        assert_eq!(port.port, 1812);
        assert_eq!(port.protocol.as_deref(), Some("UDP"));
        assert_eq!(port.target_port, Some(IntOrString::Int(1812)));
    }

    #[test]
    fn propagates_scheduling_resources_and_service_annotations() {
        let mut input = workload();
        input.resources = Some(ResourceRequirements::default());
        input.node_selector = Some(BTreeMap::from([(
            "kubernetes.io/arch".to_string(),
            "amd64".to_string(),
        )]));
        input.affinity = Some(Affinity::default());
        input.tolerations = Some(vec![Toleration::default()]);
        input.topology_spread_constraints = Some(vec![]);
        input.service_annotations.insert(
            "service.beta.kubernetes.io/example".into(),
            "radius-udp".into(),
        );
        let deployment = input.deployment().unwrap();
        let pod = deployment.spec.unwrap().template.spec.unwrap();
        assert!(pod.containers[0].resources.is_some());
        assert_eq!(
            pod.node_selector.unwrap().get("kubernetes.io/arch"),
            Some(&"amd64".to_string())
        );
        assert!(pod.affinity.is_some());
        assert_eq!(pod.tolerations.unwrap().len(), 1);
        assert_eq!(pod.topology_spread_constraints, Some(vec![]));

        let service = input.service().unwrap();
        assert_eq!(
            service
                .metadata
                .annotations
                .as_ref()
                .unwrap()
                .get("service.beta.kubernetes.io/example"),
            Some(&"radius-udp".to_string())
        );
        // Workload metadata does not inherit Service annotations.
        assert!(input.deployment().unwrap().metadata.annotations.is_none());
    }

    #[test]
    fn rejects_invalid_and_insecure_workloads() {
        let mut input = workload();
        input.replicas = 0;
        assert_eq!(
            input.deployment().unwrap_err(),
            RadiusWorkloadError::InvalidReplicas
        );
        input.replicas = 1;
        input.exposure = RadiusServiceExposure::NodePort {
            external_traffic_policy: Some("Unsupported".into()),
        };
        assert_eq!(
            input.service().unwrap_err(),
            RadiusWorkloadError::InvalidExternalTrafficPolicy
        );
    }
}
