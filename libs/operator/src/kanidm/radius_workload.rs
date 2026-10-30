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
    Capabilities, ConfigMapProjection, Container, ContainerPort, KeyToPath, PodSpec,
    PodSecurityContext, PodTemplateSpec, ProjectedVolumeSource, SeccompProfile, SecretProjection,
    SecurityContext, Service,
    ServicePort, ServiceSpec, Volume, VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use thiserror::Error;

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
                    })
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
                // radiusd account to UID 497 (checked in the bootstrap CI).
                // Explicit numeric identity is necessary for Kubelet's
                // runAsNonRoot validation when the image USER is named.
                run_as_user: Some(497),
                run_as_group: Some(497),
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
            automount_service_account_token: Some(false),
            enable_service_links: Some(false),
            security_context: Some(PodSecurityContext {
                run_as_user: Some(497),
                run_as_group: Some(497),
                run_as_non_root: Some(true),
                fs_group: Some(497),
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
                    default_mode: Some(0o440)
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
                        max_unavailable: Some(IntOrString::Int(0))
                    }),
                    ..Default::default()
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
        Ok(Service {
            metadata: self.owner_metadata(),
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
        }
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
        let security = pod.security_context.as_ref().unwrap();
        assert_eq!(security.run_as_user, Some(497));
        assert_eq!(security.run_as_non_root, Some(true));
        assert_eq!(security.fs_group, Some(497));
        assert_eq!(security.seccomp_profile.as_ref().unwrap().type_, "RuntimeDefault");

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
