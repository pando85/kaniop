//! Kubernetes workload builders for an independent Kanidm RADIUS endpoint.
//!
//! Pure construction only: reconciliation must verify parent UID ownership,
//! validate referenced inputs, and preserve last-good credentials before apply.
//! This does not mint Kanidm tokens or adopt existing Kubernetes objects.

use std::collections::BTreeMap;

use k8s_openapi::ByteString;
use k8s_openapi::api::apps::v1::{
    Deployment, DeploymentSpec, DeploymentStrategy, RollingUpdateDeployment,
};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, ConfigMapProjection, Container, ContainerPort, ExecAction, KeyToPath,
    Lifecycle, LifecycleHandler, PodSecurityContext, PodSpec, PodTemplateSpec, Probe,
    ProjectedVolumeSource, ResourceRequirements, SeccompProfile, Secret, SecretProjection,
    SecurityContext, Service, ServicePort, ServiceSpec, Toleration, TopologySpreadConstraint,
    Volume, VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{Resource, ResourceExt};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::crd::{KanidmRadiusServer, RadiusCaKind};
use crate::reconcile::config::{RadiusConfig, RadiusConfigError};
use crate::reconcile::image::{RADIUS_GID, RADIUS_UID};
use crate::reconcile::meta::{
    ANNOTATION_INPUTS_DIGEST, ANNOTATION_ROTATION_REQUEST, ANNOTATION_TOKEN_ID, COMPONENT,
    LABEL_MANAGED_BY, LABEL_NAME, LABEL_UID, MANAGED_BY, SECRET_KEY_CONFIG, SECRET_KEY_TOKEN,
};

/// Longest CR name that keeps `<name>-<hash>` a valid 63-character DNS label.
pub const MAX_RADIUS_NAME_LEN: usize = 52;
/// Hex characters of the config digest used in the config Secret name.
pub const CONFIG_HASH_LEN: usize = 10;
/// Old ReplicaSets kept for rollback. One is enough: rotation safety is
/// computed from live pods and ReplicaSets, not from history.
const REVISION_HISTORY_LIMIT: i32 = 1;
/// Seconds a terminating pod keeps serving before radiusd gets SIGTERM.
/// kube-proxy removes a pod from the Service a moment after it starts
/// terminating; UDP requests sent in between would be silently lost, which a
/// rollout with `maxUnavailable: 0` must not cause.
pub const PRE_STOP_SLEEP_SECONDS: i32 = 10;

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

impl RadiusServiceExposure {
    /// Parse the CR's Service type and traffic policy. The policy is only
    /// valid for NodePort and LoadBalancer; Kubernetes (and the CRD) reject it
    /// on ClusterIP, so it is an error here instead of being dropped silently.
    pub fn from_spec(
        service_type: &str,
        external_traffic_policy: Option<String>,
    ) -> Result<Self, RadiusWorkloadError> {
        match service_type {
            "ClusterIP" if external_traffic_policy.is_some() => {
                Err(RadiusWorkloadError::InvalidExternalTrafficPolicy)
            }
            "ClusterIP" => Ok(Self::ClusterIP),
            "NodePort" => Ok(Self::NodePort {
                external_traffic_policy,
            }),
            "LoadBalancer" => Ok(Self::LoadBalancer {
                external_traffic_policy,
            }),
            _ => Err(RadiusWorkloadError::InvalidServiceType),
        }
    }

    /// The Kubernetes `Service.spec.type` string.
    pub fn service_type(&self) -> &'static str {
        match self {
            Self::ClusterIP => "ClusterIP",
            Self::NodePort { .. } => "NodePort",
            Self::LoadBalancer { .. } => "LoadBalancer",
        }
    }

    fn external_traffic_policy(&self) -> Option<&str> {
        match self {
            Self::ClusterIP => None,
            Self::NodePort {
                external_traffic_policy,
            }
            | Self::LoadBalancer {
                external_traffic_policy,
            } => external_traffic_policy.as_deref(),
        }
    }
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
    /// Kanidm token id the config Secret was rendered with (an id, never the
    /// bearer). Recorded on the pod template so observers can tell which
    /// token each pod uses.
    pub token_id: String,
    /// Digest of the public certificate and CA bytes (see
    /// `reconcile::inputs::inputs_digest`); changing it rolls the pods.
    pub inputs_digest: String,
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
    #[error(
        "RADIUS externalTrafficPolicy must be Cluster or Local, and only applies to NodePort and LoadBalancer Services"
    )]
    InvalidExternalTrafficPolicy,
    #[error("RADIUS Service type must be ClusterIP, NodePort or LoadBalancer")]
    InvalidServiceType,
    #[error("RADIUS resource name must be at most 52 characters to derive generated names")]
    ResourceNameTooLong,
    #[error(transparent)]
    InvalidConfiguration(#[from] RadiusConfigError),
}

/// Derive the entire desired workload from the CR, the immutable config
/// Secret and the token id it was rendered with. Never derive owner identity
/// from the reusable resource name.
pub fn build_workload(
    radius: &KanidmRadiusServer,
    image: String,
    config_secret: &str,
    token_id: &str,
    inputs_digest: &str,
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
    let exposure = RadiusServiceExposure::from_spec(&radius.spec.service.type_, policy)?;
    let tls = &radius.spec.radius_tls;
    let workload = RadiusWorkload {
        name: radius.name_any(),
        namespace,
        owner,
        image,
        replicas: radius.spec.replicas,
        configuration_secret: config_secret.to_string(),
        token_id: token_id.to_string(),
        inputs_digest: inputs_digest.to_string(),
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
            || self.token_id.is_empty()
            || self.inputs_digest.is_empty()
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
        let policy = self.exposure.external_traffic_policy();
        if let Some(policy) = policy {
            if !matches!(policy, "Cluster" | "Local") {
                return Err(RadiusWorkloadError::InvalidExternalTrafficPolicy);
            }
        }
        Ok(())
    }

    fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (LABEL_NAME.into(), COMPONENT.into()),
            (LABEL_UID.into(), self.owner.uid.clone()),
        ])
    }

    fn owner_metadata(&self) -> ObjectMeta {
        // The managed-by label is metadata only: it must not enter the
        // immutable Deployment selector.
        let mut labels = self.labels();
        labels.insert(LABEL_MANAGED_BY.into(), MANAGED_BY.into());
        ObjectMeta {
            name: Some(self.name.clone()),
            namespace: Some(self.namespace.clone()),
            labels: Some(labels),
            owner_references: Some(vec![self.owner.clone()]),
            ..Default::default()
        }
    }

    /// Project all files into the single /data mount expected by the official
    /// bootstrap. No subPath mounts: each input is a user-owned Secret/ConfigMap,
    /// and the rendered configuration uses an immutable operator-owned Secret.
    fn projections(&self) -> Vec<VolumeProjection> {
        let mut projections = vec![
            VolumeProjection {
                secret: Some(SecretProjection {
                    name: self.configuration_secret.clone(),
                    items: Some(vec![KeyToPath {
                        key: SECRET_KEY_CONFIG.into(),
                        path: SECRET_KEY_CONFIG.into(),
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
            lifecycle: Some(Lifecycle {
                pre_stop: Some(LifecycleHandler {
                    exec: Some(ExecAction {
                        command: Some(vec!["sleep".into(), PRE_STOP_SLEEP_SECONDS.to_string()]),
                    }),
                    ..Default::default()
                }),
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
                // Explicit numeric identity is necessary for Kubelet's
                // runAsNonRoot validation when the image USER is named.
                run_as_user: Some(RADIUS_UID),
                run_as_group: Some(RADIUS_GID),
                run_as_non_root: Some(true),
                // readOnlyRootFilesystem is deliberately unset (plan D8). The
                // upstream bootstrap rewrites /etc/raddb/clients.conf and
                // writes /etc/raddb/certs/{ca,server}.pem, so the whole of
                // /etc/raddb must be writable. The WP-2 spike showed that a
                // read-only root works only if /etc/raddb is a writable copy
                // of the image directory, and radiusd then refuses to start
                // when that directory is world-writable, which is what an
                // emptyDir mount point is (0777). Seeding it with correct
                // permissions needs a root init container. Revisit if
                // upstream moves the generated files.
                read_only_root_filesystem: None,
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
                run_as_user: Some(RADIUS_UID),
                run_as_group: Some(RADIUS_GID),
                run_as_non_root: Some(true),
                fs_group: Some(RADIUS_GID),
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
                revision_history_limit: Some(REVISION_HISTORY_LIMIT),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels),
                        annotations: Some(BTreeMap::from([
                            (ANNOTATION_TOKEN_ID.to_string(), self.token_id.clone()),
                            (
                                ANNOTATION_INPUTS_DIGEST.to_string(),
                                self.inputs_digest.clone(),
                            ),
                        ])),
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
        let service_type = self.exposure.service_type();
        let external_traffic_policy = self.exposure.external_traffic_policy().map(String::from);
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

/// Name of the immutable config Secret: `<cr>-<first 10 hex of sha256(toml)>`.
///
/// Content addressing makes a re-render with identical content idempotent and
/// any change a new object, so pods mounting the old Secret are never mutated
/// (same practice as kustomize `secretGenerator`). The hash is over the whole
/// TOML, which embeds the bearer token and NAS secrets; 40 bits of a SHA-256
/// over a high-entropy token reveal nothing usable.
pub fn config_secret_name(cr_name: &str, radius_toml: &str) -> Result<String, RadiusWorkloadError> {
    if cr_name.is_empty() || cr_name.len() > MAX_RADIUS_NAME_LEN {
        return Err(RadiusWorkloadError::ResourceNameTooLong);
    }
    let digest = hex::encode(Sha256::digest(radius_toml.as_bytes()));
    Ok(format!("{cr_name}-{}", &digest[..CONFIG_HASH_LEN]))
}

/// Build the immutable, content-addressed config Secret.
///
/// Credentials live only in `data`. Metadata carries the owner, UID label,
/// the token id and the manual-rotation request value, none of which is
/// secret. `rotation_request` is the CR's `kaniop.rs/radius-rotate-token`
/// value at mint time.
pub fn build_config_secret(
    radius: &KanidmRadiusServer,
    token_id: &str,
    rotation_request: Option<&str>,
    config: &RadiusConfig,
) -> Result<Secret, RadiusWorkloadError> {
    let namespace = radius
        .namespace()
        .ok_or(RadiusWorkloadError::MissingResourceIdentity)?;
    let owner = radius
        .controller_owner_ref(&())
        .ok_or(RadiusWorkloadError::MissingResourceIdentity)?;
    if owner.uid.is_empty() || token_id.is_empty() {
        return Err(RadiusWorkloadError::MissingResourceIdentity);
    }
    let toml = config.render_toml()?;
    let name = config_secret_name(&radius.name_any(), &toml)?;
    let mut annotations = BTreeMap::from([(ANNOTATION_TOKEN_ID.to_string(), token_id.to_string())]);
    if let Some(request) = rotation_request {
        annotations.insert(ANNOTATION_ROTATION_REQUEST.to_string(), request.to_string());
    }
    Ok(Secret {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(namespace),
            labels: Some(BTreeMap::from([
                (LABEL_NAME.into(), COMPONENT.into()),
                (LABEL_MANAGED_BY.into(), MANAGED_BY.into()),
                (LABEL_UID.into(), owner.uid.clone()),
            ])),
            annotations: Some(annotations),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([
            (SECRET_KEY_CONFIG.into(), ByteString(toml.into_bytes())),
            (
                SECRET_KEY_TOKEN.into(),
                ByteString(config.auth_token.as_bytes().to_vec()),
            ),
        ])),
        immutable: Some(true),
        type_: Some("Opaque".into()),
        ..Default::default()
    })
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
            token_id: "token-a".into(),
            inputs_digest: "digest-a".into(),
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
        let workload = build_workload(
            &radius,
            "kanidm/radius:1.11.2".into(),
            "immutable-config",
            "token-id",
            "digest-a",
        )
        .unwrap();
        assert_eq!(workload.owner.uid, "d10066c3-4599-4487-972d-26bb42587a32");
        assert_eq!(workload.configuration_secret, "immutable-config");
        assert_eq!(workload.token_id, "token-id");
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
    fn config_secret_change_updates_pod_template_without_touching_client_secrets() {
        let a = workload().deployment().unwrap();
        let mut updated = workload();
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

    fn radius_from_service_type(service_type: &str) -> KanidmRadiusServer {
        serde_json::from_value(serde_json::json!({
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
                "service": {"type": service_type}
            }
        }))
        .unwrap()
    }

    #[test]
    fn unknown_service_type_reports_service_type_error() {
        let radius = radius_from_service_type("ExternalName");
        assert_eq!(
            build_workload(
                &radius,
                "kanidm/radius:1.11.2".into(),
                "immutable-config",
                "token-id",
                "digest-a"
            )
            .unwrap_err(),
            RadiusWorkloadError::InvalidServiceType
        );
    }

    fn radius_config() -> RadiusConfig {
        RadiusConfig {
            uri: "https://idm.example.com".into(),
            auth_token: "bearer-do-not-log".into(),
            ca_path: None,
            radius_required_groups: vec!["allowed@idm.example.com".into()],
            radius_default_vlan: 10,
            radius_groups: vec![],
            radius_clients: vec![crate::reconcile::config::RadiusClient {
                name: "nas".into(),
                ipaddr: "192.0.2.10".into(),
                secret: "safeRandomSecret123".into(),
            }],
            radius_ca_path: None,
        }
    }

    #[test]
    fn config_secret_name_is_content_addressed() {
        let a = config_secret_name("campus", "uri = \"a\"\n").unwrap();
        assert_eq!(a, config_secret_name("campus", "uri = \"a\"\n").unwrap());
        assert_ne!(a, config_secret_name("campus", "uri = \"b\"\n").unwrap());
        assert_ne!(a, config_secret_name("other", "uri = \"a\"\n").unwrap());
        let (prefix, hash) = a.rsplit_once('-').unwrap();
        assert_eq!(prefix, "campus");
        assert_eq!(hash.len(), 10);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn config_secret_name_fits_a_dns_label_for_the_longest_cr_name() {
        let longest = "a".repeat(MAX_RADIUS_NAME_LEN);
        assert_eq!(config_secret_name(&longest, "x").unwrap().len(), 63);
        assert_eq!(
            config_secret_name(&"a".repeat(MAX_RADIUS_NAME_LEN + 1), "x"),
            Err(RadiusWorkloadError::ResourceNameTooLong)
        );
    }

    #[test]
    fn config_secret_is_immutable_and_keeps_credentials_out_of_metadata() {
        let radius = radius_from_service_type("ClusterIP");
        let config = radius_config();
        let secret = build_config_secret(&radius, "token-a", Some("req-1"), &config).unwrap();
        let toml = config.render_toml().unwrap();
        assert_eq!(
            secret.metadata.name.as_deref(),
            Some(config_secret_name("campus-radius", &toml).unwrap().as_str())
        );
        assert_eq!(secret.immutable, Some(true));
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        let data = secret.data.as_ref().unwrap();
        assert_eq!(data[SECRET_KEY_CONFIG].0, toml.as_bytes());
        assert_eq!(data[SECRET_KEY_TOKEN].0, b"bearer-do-not-log");

        let annotations = secret.metadata.annotations.as_ref().unwrap();
        assert_eq!(annotations[ANNOTATION_TOKEN_ID], "token-a");
        assert_eq!(annotations[ANNOTATION_ROTATION_REQUEST], "req-1");
        // Names, labels, annotations and owner references: no credential.
        let metadata = serde_json::to_string(&secret.metadata).unwrap();
        assert!(!metadata.contains("bearer-do-not-log"));
        assert!(!metadata.contains("safeRandomSecret123"));
        assert_eq!(
            secret.metadata.owner_references.as_ref().unwrap()[0].uid,
            "d10066c3-4599-4487-972d-26bb42587a32"
        );
    }

    #[test]
    fn config_secret_name_follows_the_rendered_config() {
        let radius = radius_from_service_type("ClusterIP");
        let mut config = radius_config();
        let a = build_config_secret(&radius, "token-a", None, &config).unwrap();
        let same = build_config_secret(&radius, "token-a", None, &config).unwrap();
        assert_eq!(a.metadata.name, same.metadata.name);
        assert!(
            !a.metadata
                .annotations
                .as_ref()
                .unwrap()
                .contains_key(ANNOTATION_ROTATION_REQUEST)
        );
        config.radius_clients[0].secret = "anotherSafeSecret456".into();
        let b = build_config_secret(&radius, "token-a", None, &config).unwrap();
        assert_ne!(a.metadata.name, b.metadata.name);
    }

    #[test]
    fn pod_template_carries_token_id_and_inputs_digest() {
        let deployment = workload().deployment().unwrap();
        let template = deployment.spec.unwrap().template;
        let annotations = template.metadata.unwrap().annotations.unwrap();
        assert_eq!(annotations[ANNOTATION_TOKEN_ID], "token-a");
        assert_eq!(annotations[ANNOTATION_INPUTS_DIGEST], "digest-a");

        let mut renewed = workload();
        renewed.inputs_digest = "digest-b".into();
        assert_ne!(
            deployment_template(&workload()),
            deployment_template(&renewed),
            "TLS renewal must roll the pods"
        );
        let mut rotated = workload();
        rotated.token_id = "token-b".into();
        assert_ne!(
            deployment_template(&workload()),
            deployment_template(&rotated)
        );
    }

    fn deployment_template(workload: &RadiusWorkload) -> PodTemplateSpec {
        workload.deployment().unwrap().spec.unwrap().template
    }

    #[test]
    fn workload_requires_token_id_and_inputs_digest() {
        let mut input = workload();
        input.token_id.clear();
        assert_eq!(
            input.deployment().unwrap_err(),
            RadiusWorkloadError::MissingResourceIdentity
        );
        let mut input = workload();
        input.inputs_digest.clear();
        assert_eq!(
            input.deployment().unwrap_err(),
            RadiusWorkloadError::MissingResourceIdentity
        );
    }

    #[test]
    fn terminating_pods_keep_serving_while_endpoint_removal_propagates() {
        let pod = deployment_template(&workload()).spec.unwrap();
        let hook = pod.containers[0]
            .lifecycle
            .as_ref()
            .and_then(|l| l.pre_stop.as_ref())
            .and_then(|h| h.exec.as_ref())
            .and_then(|e| e.command.clone());
        assert_eq!(
            hook,
            Some(vec!["sleep".into(), PRE_STOP_SLEEP_SECONDS.to_string()])
        );
        // The grace period must outlast the hook, or radiusd is killed first.
        assert!(pod.termination_grace_period_seconds.unwrap() > i64::from(PRE_STOP_SLEEP_SECONDS));
    }

    #[test]
    fn deployment_keeps_one_old_revision() {
        let spec = workload().deployment().unwrap().spec.unwrap();
        assert_eq!(spec.revision_history_limit, Some(1));
    }

    #[test]
    fn generated_objects_are_labelled_but_the_selector_is_stable() {
        let input = workload();
        let deployment = input.deployment().unwrap();
        let labels = deployment.metadata.labels.as_ref().unwrap();
        assert_eq!(labels[LABEL_MANAGED_BY], "kaniop-radius-server");
        assert_eq!(
            input.service().unwrap().metadata.labels.as_ref().unwrap()[LABEL_MANAGED_BY],
            "kaniop-radius-server"
        );
        let selector = deployment.spec.unwrap().selector.match_labels.unwrap();
        assert_eq!(
            selector.keys().map(String::as_str).collect::<Vec<_>>(),
            [LABEL_NAME, LABEL_UID]
        );
    }

    #[test]
    fn runtime_identity_uses_the_verified_numeric_ids() {
        assert_eq!((RADIUS_UID, RADIUS_GID), (497, 496));
        let pod = workload()
            .deployment()
            .unwrap()
            .spec
            .unwrap()
            .template
            .spec
            .unwrap();
        let container = pod.containers[0].security_context.as_ref().unwrap();
        assert_eq!(container.run_as_user, Some(RADIUS_UID));
        assert_eq!(container.run_as_group, Some(RADIUS_GID));
        let pod_security = pod.security_context.unwrap();
        assert_eq!(pod_security.run_as_user, Some(RADIUS_UID));
        assert_eq!(pod_security.fs_group, Some(RADIUS_GID));
    }

    #[test]
    fn root_filesystem_stays_writable_because_upstream_bootstraps_into_etc_raddb() {
        // Plan D8, WP-2 spike: the image rewrites /etc/raddb/clients.conf and
        // writes /etc/raddb/certs/*.pem at startup, and radiusd refuses to run
        // from a world-writable /etc/raddb (an emptyDir mount point is 0777).
        // Re-evaluate this assertion only together with the image contract.
        let pod = workload()
            .deployment()
            .unwrap()
            .spec
            .unwrap()
            .template
            .spec
            .unwrap();
        let container = pod.containers[0].security_context.as_ref().unwrap();
        assert_eq!(container.read_only_root_filesystem, None);
    }

    #[test]
    fn service_type_parses_every_supported_value_and_nothing_else() {
        for (value, expected) in [
            ("ClusterIP", "ClusterIP"),
            ("NodePort", "NodePort"),
            ("LoadBalancer", "LoadBalancer"),
        ] {
            let exposure = RadiusServiceExposure::from_spec(value, None).unwrap();
            assert_eq!(exposure.service_type(), expected);
        }
        assert_eq!(
            RadiusServiceExposure::from_spec("clusterip", None),
            Err(RadiusWorkloadError::InvalidServiceType)
        );
    }

    #[test]
    fn external_traffic_policy_is_rejected_on_cluster_ip() {
        assert_eq!(
            RadiusServiceExposure::from_spec("ClusterIP", Some("Local".into())),
            Err(RadiusWorkloadError::InvalidExternalTrafficPolicy)
        );
        for value in ["NodePort", "LoadBalancer"] {
            assert!(RadiusServiceExposure::from_spec(value, Some("Local".into())).is_ok());
        }
    }
}
