use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::StatefulSetPersistentVolumeClaimRetentionPolicy;
use k8s_openapi::api::core::v1::{
    Affinity, Container, EmptyDirVolumeSource, EphemeralVolumeSource, HostAlias,
    PersistentVolumeClaimTemplate, PodDNSConfig, PodSecurityContext, ResourceRequirements,
    Toleration, TopologySpreadConstraint, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Specification of the desired behavior of the Kanidm cluster. More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
// workaround: '`' character is not allowed in the kube `doc` attribute during doctests
#[cfg_attr(not(doctest), kube(doc = r#"The `Kanidm` custom resource definition (CRD) defines a desired [Kanidm](https://kanidm.com)
    setup to run in a Kubernetes cluster. It allows to specify many options such as the number of replicas,
    persistent storage, and many more.

    For each `Kanidm` resource, the Operator deploys one or several `StatefulSet` objects in the same namespace. The number of StatefulSets is equal to the number of replicas.
    "#))]
#[kube(
    group = "kaniop.rs",
    version = "v1",
    kind = "Kanidm",
    plural = "kanidms",
    singular = "kanidm",
    shortname = "idm",
    namespaced,
    status = "KanidmStatus",
    printcolumn = r#"{"name":"Desired","type":"integer","description":"The number of desired replicas","jsonPath":".status.replicas"}"#,
    printcolumn = r#"{"name":"Ready","type":"integer","description":"The number of ready replicas","jsonPath":".status.availableReplicas"}"#,
    printcolumn = r#"{"name":"Reconciled","type":"string","jsonPath":".status.conditions[?(@.type == 'Reconciled')].status"}"#,
    printcolumn = r#"{"name":"Available","type":"string","jsonPath":".status.conditions[?(@.type == 'Available')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmSpec {
    /// The DNS domain name of the server. This is used in a number of security-critical
    /// contexts such as webauthn, so it *must* match your DNS hostname. It is used to
    /// create security principal names such as `william@idm.example.com` so that in a
    /// (future) trust configuration it is possible to have unique Security Principal
    /// Names (spns) throughout the topology.
    // TODO: wait for schemars 1.0.0 and k8s-openapi implements it
    // schemars = 1.0.0
    //#[schemars(extend("x-kubernetes-validations" = [{"message": "Value is immutable", "rule": "self == oldSelf"}]))]
    #[schemars(regex(pattern = r"^(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$"))]
    pub domain: String,
    /// Container image name. More info: https://kubernetes.io/docs/concepts/containers/images This field is optional to allow higher level config management to default or override container images in workload controllers like Deployments and StatefulSets.
    #[serde(default = "default_image")]
    pub image: String,
    /// Image pull policy. One of Always, Never, IfNotPresent. Defaults to Always if :latest tag is specified, or IfNotPresent otherwise. Cannot be updated. More info: https://kubernetes.io/docs/concepts/containers/images#updating-images
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_pull_policy: Option<String>,
    /// Log level for Kanidm.
    #[serde(default)]
    pub log_level: KanidmLogLevel,
    /// Number of replicas to deploy for a Kanidm deployment.
    #[serde(default = "default_replicas")]
    #[validate(range(max = 2))]
    pub replicas: i32,
    /// Storage defines the storage used by Kanidm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<KanidmStorage>,
    /// Volumes allows the configuration of additional volumes on the output StatefulSet definition. Volumes specified will be appended to other volumes that are generated as a result of StorageSpec objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumes: Option<Vec<Volume>>,
    /// VolumeMounts allows the configuration of additional VolumeMounts.
    ///
    /// VolumeMounts will be appended to other VolumeMounts in the kanidm’ container, that are generated as a result of StorageSpec objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_mounts: Option<Vec<VolumeMount>>,
    /// The field controls if and how PVCs are deleted during the lifecycle of a StatefulSet. The default behavior is all PVCs are retained. This is an alpha field from kubernetes 1.23 until 1.26 and a beta field from 1.26. It requires enabling the StatefulSetAutoDeletePVC feature gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent_volume_claim_retention_policy:
        Option<StatefulSetPersistentVolumeClaimRetentionPolicy>,
    /// Defines the resources requests and limits of the kanidm’ container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    /// Defines on which Nodes the Pods are scheduled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,
    /// Defines the Pods’ affinity scheduling rules if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,
    /// Defines the Pods’ tolerations if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerations: Option<Vec<Toleration>>,
    /// Defines the pod’s topology spread constraints if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_spread_constraints: Option<Vec<TopologySpreadConstraint>>,
    /// SecurityContext holds pod-level security attributes and common container settings. This defaults to the default PodSecurityContext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_context: Option<PodSecurityContext>,
    /// Defines the DNS policy for the pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_policy: Option<String>,
    /// Defines the DNS configuration for the pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_config: Option<PodDNSConfig>,
    /// Containers allows injecting additional containers or modifying operator generated
    /// containers. This can be used to allow adding an authentication proxy to the Pods or to
    /// change the behavior of an operator generated container. Containers described here modify
    /// an operator generated container if they share the same name and modifications are done
    /// via a strategic merge patch.
    ///
    /// The name of container managed by the operator is: kanidm
    ///
    /// Overriding containers is entirely outside the scope of what the maintainers will support
    /// and by doing so, you accept that this behaviour may break at any time without notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub containers: Option<Vec<Container>>,
    /// InitContainers allows injecting initContainers to the Pod definition. Those can be used to
    /// e.g. fetch secrets for injection into the Kanidm configuration from external sources.
    /// Any errors during the execution of an initContainer will lead to a restart of the Pod.
    /// More info: https://kubernetes.io/docs/concepts/workloads/pods/init-containers/
    /// InitContainers described here modify an operator generated init containers if they share
    /// the same name and modifications are done via a strategic merge patch.
    ///
    /// The names of init container name managed by the operator are: * init-config-reloader.
    ///
    /// Overriding init containers is entirely outside the scope of what the maintainers will
    /// support and by doing so, you accept that this behaviour may break at any time without notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_containers: Option<Vec<Container>>,
    /// Port name used for the pods and governing service. Default: "https"
    #[serde(default = "default_port_name")]
    pub port_name: String,
    /// Minimum number of seconds for which a newly created Pod should be ready without any of its
    /// container crashing for it to be considered available. Defaults to 0 (pod will be considered
    /// available as soon as it is ready)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ready_seconds: Option<i32>,
    /// Optional list of hosts and IPs that will be injected into the Pod’s hosts file if specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_aliases: Option<Vec<HostAlias>>,
    /// Use the host’s network namespace if true.
    ///
    /// Make sure to understand the security implications if you want to enable it
    /// (https://kubernetes.io/docs/concepts/configuration/overview/).
    ///
    /// When hostNetwork is enabled, this will set the DNS policy to ClusterFirstWithHostNet
    /// automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_network: Option<bool>,
    /// Defines the maximum time that the kanidm container’s startup probe will wait before
    /// being considered failed. The startup probe will return success after the WAL replay is
    /// complete. If set, the value should be greater than 60 (seconds). Otherwise it will be
    /// equal to 600 seconds (15 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_startup_duration_seconds: Option<i32>,
}

fn default_image() -> String {
    "kanidm/server:latest".to_string()
}

fn default_replicas() -> i32 {
    1
}

// reimplmentation of sketching::LogLevel because it is not Serialize
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum KanidmLogLevel {
    Trace,
    Debug,
    #[default]
    Info,
}

fn default_port_name() -> String {
    "https".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmStorage {
    /// EmptyDirVolumeSource to be used by the StatefulSet. If specified, it takes precedence over `ephemeral` and `volumeClaimTemplate`. More info: https://kubernetes.io/docs/concepts/storage/volumes/#emptydir
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_dir: Option<EmptyDirVolumeSource>,
    /// EphemeralVolumeSource to be used by the StatefulSet. This is a beta field in k8s 1.21 and GA in 1.15. For lower versions, starting with k8s 1.19, it requires enabling the GenericEphemeralVolume feature gate. More info: https://kubernetes.io/docs/concepts/storage/ephemeral-volumes/#generic-ephemeral-volumes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<EphemeralVolumeSource>,
    /// Defines the PVC spec to be used by the Kanidm StatefulSets. The easiest way to use a volume that cannot be automatically provisioned is to use a label selector alongside manually created PersistentVolumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_claim_template: Option<PersistentVolumeClaimTemplate>,
}

/// Most recent observed status of the Kanidm cluster. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmStatus {
    /// Total number of available pods (ready for at least minReadySeconds) targeted by this Kanidm deployment.
    pub available_replicas: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,
    /// Total number of non-terminated pods targeted by this Kanidm deployment.
    pub replicas: i32,
    /// Total number of unavailable pods targeted by this Kanidm deployment.
    pub unavailable_replicas: i32,
    /// Total number of non-terminated pods targeted by this Kanidm deployment that have the desired version spec.
    pub updated_replicas: i32,
}
