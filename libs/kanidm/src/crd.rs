use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::StatefulSetPersistentVolumeClaimRetentionPolicy;
use k8s_openapi::api::core::v1::{
    Affinity, Container, EmptyDirVolumeSource, EnvVar, EphemeralVolumeSource, HostAlias,
    PersistentVolumeClaim, PodDNSConfig, PodSecurityContext, ResourceRequirements,
    SecretKeySelector, Toleration, TopologySpreadConstraint, Volume, VolumeMount,
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
#[cfg_attr(
    not(doctest),
    kube(
        doc = r#"The `Kanidm` custom resource definition (CRD) defines a desired [Kanidm](https://kanidm.com)
    setup to run in a Kubernetes cluster. It allows to specify many options such as the number of replicas,
    persistent storage, and many more.

    For each `Kanidm` resource, the Operator deploys one or several `StatefulSet` objects in the same namespace. The number of StatefulSets is equal to the number of replicas.
    "#
    )
)]
#[kube(
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "Kanidm",
    plural = "kanidms",
    singular = "kanidm",
    shortname = "idm",
    namespaced,
    status = "KanidmStatus",
    printcolumn = r#"{"name":"Desired","type":"integer","description":"The number of desired replicas","jsonPath":".status.replicas"}"#,
    printcolumn = r#"{"name":"Ready","type":"integer","description":"The number of ready replicas","jsonPath":".status.availableReplicas"}"#,
    printcolumn = r#"{"name":"Available","type":"string","jsonPath":".status.conditions[?(@.type == 'Available')].status"}"#,
    printcolumn = r#"{"name":"Progressing","type":"string","jsonPath":".status.conditions[?(@.type == 'Progressing')].status"}"#,
    printcolumn = r#"{"name":"Initialized","type":"string","jsonPath":".status.conditions[?(@.type == 'Initialized')].status"}"#,
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
    ///
    /// This cannot be changed after creation.
    // TODO: move from ValidatingAdmissionPolicy to here when schemars 1.0.0 is released and k8s-openapi implements it
    // schemars = 1.0.0
    //#[schemars(extend("x-kubernetes-validations" = [{"message": "Value is immutable", "rule": "self == oldSelf"}]))]
    #[schemars(regex(
        pattern = r"^([a-z0-9]([-a-z0-9]*[a-z0-9])?\.)*[a-z0-9]([-a-z0-9]*[a-z0-9])?$"
    ))]
    pub domain: String,

    /// Different group of replicas with specific configuration as role, resources, affinity rules, and more.
    /// Each group will be deployed as a separate StatefulSet.
    // TODO: move from ValidatingAdmissionPolicy to here when schemars 1.0.0 is released
    //#[schemars(extend("x-kubernetes-validations" = [{"message": "Value is immutable", "rule": "self.size() > 0"}]))]
    // max is defined for allowing CEL expression in validation admission policy estimate
    // expression costs
    #[validate(length(min = 1, max = 100))]
    pub replica_groups: Vec<ReplicaGroup>,

    /// List of external replication nodes. This is used to configure replication between
    /// different Kanidm clusters.
    // max is defined for allowing CEL expression in validation admission policy estimate
    // expression costs
    #[validate(length(max = 100))]
    #[serde(default)]
    pub external_replication_nodes: Vec<ExternalReplicationNode>,

    /// Container image name. More info: https://kubernetes.io/docs/concepts/containers/images
    /// This field is optional to allow higher level config management to default or override
    /// container images in workload controllers like StatefulSets.
    #[serde(default = "default_image")]
    pub image: String,

    /// Image pull policy. One of Always, Never, IfNotPresent. Defaults to Always if :latest tag
    /// is specified, or IfNotPresent otherwise. Cannot be updated.
    /// More info: https://kubernetes.io/docs/concepts/containers/images#updating-images
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_pull_policy: Option<String>,

    /// Log level for Kanidm.
    #[serde(default)]
    pub log_level: KanidmLogLevel,

    /// List of environment variables to set in the `kanidm` container.
    /// This can be used to set Kanidm configuration options.
    /// More info: https://kanidm.github.io/kanidm/master/server_configuration.html
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVar>>,

    /// StorageSpec defines the configured storage for a group Kanidm servers.
    /// If no storage option is specified, then by default an
    /// [EmptyDir](https://kubernetes.io/docs/concepts/storage/volumes/#emptydir) will be used.
    ///
    /// If multiple storage options are specified, priority will be given as follows:
    ///  1. emptyDir
    ///  2. ephemeral
    ///  3. volumeClaimTemplate
    ///
    /// Note: Kaniop does not resize PVCs until Kubernetes fix
    /// [KEP-4650](https://github.com/kubernetes/enhancements/pull/4651).
    /// Although, StatefulSet will be recreated if the PVC is resized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<KanidmStorage>,

    /// Defines the port name used for the LDAP service. If not defined, LDAP service will not be
    /// configured. Service port will be `3636`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ldap_port_name: Option<String>,

    /// Specifies the name of the secret holding the TLS private key and certificate for the server.
    /// If not provided, the ingress secret will be used. The server will not start if the secret
    /// is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_secret_name: Option<String>,

    /// Service defines the service configuration for the Kanidm server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<KanidmService>,

    /// Ingress defines the ingress configuration for the Kanidm server. Domain will be the host
    /// for the ingress. TLS is required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingress: Option<KanidmIngress>,

    /// Volumes allows the configuration of additional volumes on the output StatefulSet
    /// definition. Volumes specified will be appended to other volumes that are generated as a
    /// result of StorageSpec objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<Vec<Volume>>,

    /// VolumeMounts allows the configuration of additional VolumeMounts.
    ///
    /// VolumeMounts will be appended to other VolumeMounts in the kanidm’ container, that are
    /// generated as a result of StorageSpec objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_mounts: Option<Vec<VolumeMount>>,

    /// The field controls if and how PVCs are deleted during the lifecycle of a StatefulSet.
    /// The default behavior is all PVCs are retained.
    /// This is a beta field from 1.27. It requires enabling the StatefulSetAutoDeletePVC feature
    /// gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent_volume_claim_retention_policy:
        Option<StatefulSetPersistentVolumeClaimRetentionPolicy>,

    /// SecurityContext holds pod-level security attributes and common container settings.
    /// This defaults to the default PodSecurityContext.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_context: Option<PodSecurityContext>,

    /// Defines the DNS policy for the pods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_policy: Option<String>,

    /// Defines the DNS configuration for the pods.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_containers: Option<Vec<Container>>,

    /// Port name used for the pods and governing service. Default: "https"
    #[serde(default = "default_port_name")]
    pub port_name: String,

    /// Minimum number of seconds for which a newly created Pod should be ready without any of its
    /// container crashing for it to be considered available. Defaults to 0 (pod will be considered
    /// available as soon as it is ready)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ready_seconds: Option<i32>,

    /// Optional list of hosts and IPs that will be injected into the Pod’s hosts file if specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_aliases: Option<Vec<HostAlias>>,

    /// Use the host’s network namespace if true.
    ///
    /// Make sure to understand the security implications if you want to enable it
    /// (https://kubernetes.io/docs/concepts/configuration/overview/).
    ///
    /// When hostNetwork is enabled, this will set the DNS policy to ClusterFirstWithHostNet
    /// automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_network: Option<bool>,

    /// Defines the maximum time that the kanidm container’s startup probe will wait before
    /// being considered failed. The startup probe will return success after the WAL replay is
    /// complete. If set, the value should be greater than 60 (seconds). Otherwise it will be
    /// equal to 600 seconds (15 minutes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_startup_duration_seconds: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReplicaGroup {
    /// The name of the replica group.
    pub name: String,

    /// Number of replicas to deploy for a Kanidm replica group.
    pub replicas: i32,

    /// The Kanidm role of each node in the replica group.
    #[serde(default)]
    pub role: KanidmServerRole,

    /// If true, the first pod of the StatefulSet will be considered as the primary node.
    /// The rest of the nodes are considered as secondary nodes.
    /// This means that if database issues occur the content of the primary will take precedence
    /// over the rest of the nodes.
    /// This is only valid for the WriteReplica role and can only be set to true for one
    /// replica group or external replication node.
    /// Defaults to false.
    #[serde(default)]
    pub primary_node: bool,

    /// Defines the resources requests and limits of the kanidm’ container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    /// Defines on which Nodes the Pods are scheduled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,

    /// Defines the Pods’ affinity scheduling rules if specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,

    /// Defines the Pods’ tolerations if specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tolerations: Option<Vec<Toleration>>,

    /// Defines the pod’s topology spread constraints if specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_spread_constraints: Option<Vec<TopologySpreadConstraint>>,
}

// re-implementation of kanidmd_core::config::ServerRole because it is not Serialize
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum KanidmServerRole {
    #[default]
    WriteReplica,
    WriteReplicaNoUI,
    ReadOnlyReplica,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ExternalReplicationNode {
    /// Name of the external replication node. This just have internal use.
    pub name: String,

    /// The hostname of the external replication node.
    pub hostname: String,

    /// The replication port of the external replication node.
    pub port: i32,

    /// Defines the secret that contains the identity certificate of the external replication node.
    pub certificate: SecretKeySelector,

    /// Defines the type of replication to use. Defaults to MutualPull.
    #[serde(default)]
    pub _type: ReplicationType,

    /// Select external replication node as the primary node. This means that if database conflicts
    /// occur the content of the primary will take precedence over the rest of the nodes.
    /// Note: just one external replication node or replication group can be selected as primary.
    /// Defaults to false.
    #[serde(default)]
    pub automatic_refresh: bool,
}

// re-implementation of kanidmd_core::repl::config::RepNodeConfig because it is not Serialize and
// attributes changed
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ReplicationType {
    #[default]
    MutualPull,
    AllowPull,
    Pull,
}

fn default_image() -> String {
    "kanidm/server:latest".to_string()
}

// re-implementation of sketching::LogLevel because it is not Serialize
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
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
    /// EmptyDirVolumeSource to be used by the StatefulSet. If specified, it takes precedence over
    /// `ephemeral` and `volumeClaimTemplate`.
    /// More info: https://kubernetes.io/docs/concepts/storage/volumes/#emptydir
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_dir: Option<EmptyDirVolumeSource>,

    /// EphemeralVolumeSource to be used by the StatefulSet.
    /// More info: https://kubernetes.io/docs/concepts/storage/ephemeral-volumes/#generic-ephemeral-volumes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<EphemeralVolumeSource>,

    /// Defines the PVC spec to be used by the Kanidm StatefulSets. The easiest way to use a volume
    /// that cannot be automatically provisioned is to use a label selector alongside manually
    /// created PersistentVolumes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_claim_template: Option<PersistentVolumeClaim>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmService {
    /// Annotations is an unstructured key value map stored with a resource that may be set by
    /// external tools to store and retrieve arbitrary metadata. They are not queryable and should
    /// be preserved when modifying objects.
    /// More info: https://kubernetes.io/docs/concepts/overview/working-with-objects/annotations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,

    /// Specify the Service's type where the Kanidm Service is exposed
    /// Please note that some Ingress controllers like https://github.com/kubernetes/ingress-gce
    /// forces you to expose your Service on a NodePort
    /// Defaults to ClusterIP. Valid options are ExternalName, ClusterIP, NodePort, and
    /// LoadBalancer. "ClusterIP" allocates a cluster-internal IP address for load-balancing to
    /// endpoints. Endpoints are determined by the selector or if that is not specified, by manual
    /// construction of an Endpoints object or EndpointSlice objects. If clusterIP is "None",
    /// no virtual IP is allocated and the endpoints are published as a set of endpoints rather
    /// than a virtual IP. "NodePort" builds on ClusterIP and allocates a port on every node which
    /// routes to the same endpoints as the clusterIP. "LoadBalancer" builds on NodePort and creates
    /// an external load-balancer (if supported in the current cloud) which routes to the same
    /// endpoints as the clusterIP. "ExternalName" aliases this service to the specified
    /// externalName. Several other fields do not apply to ExternalName services.
    /// More info: https://kubernetes.io/docs/concepts/services-networking/service/#publishing-services-service-types
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmIngress {
    /// Annotations is an unstructured key value map stored with a resource that may be set by
    /// external tools to store and retrieve arbitrary metadata. They are not queryable and should
    /// be preserved when modifying objects.
    /// More info: https://kubernetes.io/docs/concepts/overview/working-with-objects/annotations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,

    /// ingressClassName is the name of an IngressClass cluster resource. Ingress controller
    /// implementations use this field to know whether they should be serving this Ingress resource,
    /// by a transitive connection (controller -\> IngressClass -\> Ingress resource). Although the
    /// `kubernetes.io/ingress.class` annotation (simple constant name) was never formally defined,
    /// it was widely supported by Ingress controllers to create a direct binding between Ingress
    /// controller and Ingress resources. Newly created Ingress resources should prefer using the
    /// field. However, even though the annotation is officially deprecated, for backwards
    /// compatibility reasons, ingress controllers should still honor that annotation if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingress_class_name: Option<String>,
    /// Defines the name of the secret that contains the TLS private key and certificate for the
    /// server. If not defined, the default will be the Kanidm name appended with `-tls`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_secret_name: Option<String>,
}

/// Most recent observed status of the Kanidm cluster. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmStatus {
    /// Total number of available pods (ready for at least minReadySeconds) targeted by this Kanidm
    /// deployment.
    pub available_replicas: i32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// Total number of non-terminated pods targeted by this Kanidm cluster.
    pub replicas: i32,

    /// Total number of unavailable pods targeted by this Kanidm cluster.
    pub unavailable_replicas: i32,

    /// Total number of non-terminated pods targeted by this Kanidm cluster that have the
    /// desired version spec.
    pub updated_replicas: i32,

    /// Status per replica in the Kanidm cluster.
    pub replica_statuses: Vec<KanidmReplicaStatus>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmReplicaStatus {
    /// Pod name: replica group StatefulSet name plus the pod index.
    pub pod_name: String,

    /// StatefulSet name: Kanidm name plus the replica group name.
    pub statefulset_name: String,

    /// The current state of the replica.
    pub state: KanidmReplicaState,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum KanidmReplicaState {
    Initialized,
    Pending,
}
