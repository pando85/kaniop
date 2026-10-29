//! Proposed public API for independently managed Kanidm RADIUS endpoints.
//!
//! This is the *source* of the CRD. Generated Kubernetes CRD YAML and example
//! manifests must be produced by the repository's Rust generators, never
//! hand-edited. Runtime reconciliation and registration are separate gates.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Affinity, ResourceRequirements, Toleration, TopologySpreadConstraint,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::KanidmRef;
use crate::kanidm::crd::CaBundleRef;

/// An independently deployed Kanidm-backed RADIUS authentication endpoint.
///
/// RADIUS user credentials are managed inside Kanidm by each account, not in
/// this resource. The operator creates one dedicated read-only service account,
/// config Secret, Deployment and UDP Service per resource.
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmRadiusServer",
    plural = "kanidmradiusservers",
    singular = "kanidmradiusserver",
    shortname = "krs",
    namespaced,
    status = "KanidmRadiusServerStatus",
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".spec.kanidmRef.name"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmRadiusServerSpec {
    /// Target Kanidm cluster. Cross-namespace access is subject to the
    /// referenced Kanidm namespace selector. Immutable after creation.
    pub kanidm_ref: KanidmRef,

    /// Optional RADIUS component image override, independent from the Kanidm
    /// server image. Required if the Kanidm image uses a digest or an unknown
    /// registry. Do not inherit the server image digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Number of RADIUS replicas, at least one. Defaults to two.
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Authorization groups and VLAN mapping. At least one required group must
    /// be present to avoid inadvertently permitting all Kanidm users.
    pub authorization: RadiusAuthorizationSpec,

    /// Allowed NAS clients. Never put the RADIUS shared secret in the CRD:
    /// only references to same-namespace Kubernetes Secret keys are accepted.
    pub clients: Vec<RadiusClientSpec>,

    /// User-managed EAP certificate and key. Required by upstream bootstrap.
    pub radius_tls: RadiusTlsSpec,

    /// Optional CA bundle for outbound Kanidm HTTPS certificate verification.
    /// Verification of TLS hostnames and certificates always remains enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_tls: Option<KanidmTlsSpec>,

    /// UDP Service endpoint; defaults to ClusterIP:1812.
    #[serde(default)]
    pub service: RadiusServiceSpec,

    /// Periodic API token rotation (disabled unless explicitly enabled).
    #[serde(default)]
    pub token_rotation: RadiusTokenRotationSpec,

    /// Resource requests and limits for the official RADIUS container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    /// Optional scheduling controls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tolerations: Option<Vec<Toleration>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_spread_constraints: Option<Vec<TopologySpreadConstraint>>,
}

fn default_replicas() -> i32 {
    2
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusAuthorizationSpec {
    /// Users must belong to at least one of these Kanidm groups.
    pub required_groups: Vec<String>,

    /// Fallback VLAN for permitted users. Valid production VLAN range: 1..4094.
    #[serde(default = "default_vlan")]
    pub default_vlan: i32,

    /// Optional per-group VLAN mappings, independent of authorization groups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_vlans: Vec<RadiusGroupVlan>,
}

fn default_vlan() -> i32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusGroupVlan {
    /// Kanidm group SPN to map to a VLAN.
    pub group: String,
    pub vlan: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusClientSpec {
    /// FreeRADIUS client name; safe ASCII letters, numbers, dash, underscore.
    pub name: String,

    /// IPv4/IPv6 address or nonzero CIDR identifying the connecting NAS.
    pub ipaddr: String,

    /// Reference to a same-namespace Secret key. Values are validated again
    /// after reading to prevent injection into FreeRADIUS clients.conf.
    pub shared_secret_ref: RadiusSecretKeyRef,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusSecretKeyRef {
    pub name: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmTlsSpec {
    /// Reference to a CA bundle for the Kanidm HTTPS endpoint. The operator
    /// never disables hostname or certificate validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_bundle_ref: Option<CaBundleRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusSecretRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusTlsSpec {
    /// Same-namespace Secret with EAP server certificate and private key.
    pub secret_ref: RadiusSecretRef,

    /// Key containing the PEM certificate chain, defaults to tls.crt.
    #[serde(default = "default_cert_key")]
    pub cert_key: String,

    /// Key containing the matching PEM private key, defaults to tls.key.
    #[serde(default = "default_private_key")]
    pub private_key_key: String,

    /// Optional key in the same Secret containing the trusted EAP CA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_key: Option<String>,
}

fn default_cert_key() -> String {
    "tls.crt".into()
}

fn default_private_key() -> String {
    "tls.key".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusServiceSpec {
    /// ClusterIP, NodePort or LoadBalancer. Defaults to ClusterIP.
    #[serde(rename = "type", default = "default_service_type")]
    pub type_: String,

    /// Exposed UDP Service port, defaults to 1812.
    #[serde(default = "default_service_port")]
    pub port: i32,

    /// Cluster or Local, only meaningful for NodePort and LoadBalancer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_traffic_policy: Option<String>,

    /// Annotations to place on the Service (for cloud load balancers).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl Default for RadiusServiceSpec {
    fn default() -> Self {
        Self {
            type_: default_service_type(),
            port: default_service_port(),
            external_traffic_policy: None,
            annotations: BTreeMap::new(),
        }
    }
}

fn default_service_type() -> String {
    "ClusterIP".into()
}

fn default_service_port() -> i32 {
    1812
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusTokenRotationSpec {
    /// Opt in to automatic rotation. A new read-only token must be proven
    /// usable before the existing token is revoked.
    #[serde(default)]
    pub enabled: bool,

    /// Rotation interval, in days. Defaults to 30.
    #[serde(default = "default_rotation_days")]
    pub period_days: i32,
}

fn default_rotation_days() -> i32 {
    30
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRadiusServerStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Option<Vec<Condition>>,

    #[serde(default)]
    pub ready: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_config_secret: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_token_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_token_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_replicas: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_explicit_and_are_not_permissive() {
        assert_eq!(RadiusServiceSpec::default().type_, "ClusterIP");
        assert_eq!(RadiusServiceSpec::default().port, 1812);
        assert_eq!(RadiusTokenRotationSpec::default().enabled, false);
        assert_eq!(default_replicas(), 2);
    }

    #[test]
    fn nas_secrets_are_references_only() {
        let value: RadiusClientSpec = serde_json::from_value(serde_json::json!({
            "name": "mikrotik",
            "ipaddr": "192.0.2.10",
            "sharedSecretRef": {"name": "nas-credentials", "key": "mikrotik"}
        }))
        .unwrap();
        assert_eq!(value.shared_secret_ref.name, "nas-credentials");
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("sharedSecret\""));
        assert!(serialized.contains("sharedSecretRef"));
        let raw_secret = serde_json::json!({
            "name": "mikrotik",
            "ipaddr": "192.0.2.10",
            "sharedSecret": "do-not-store-this",
            "sharedSecretRef": {"name": "nas-credentials", "key": "mikrotik"}
        });
        assert!(serde_json::from_value::<RadiusClientSpec>(raw_secret).is_err());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn conditions_have_map_list_schema_and_refs_are_immutable() {
        use kube::CustomResourceExt;

        let crd = KanidmRadiusServer::crd();
        let json = serde_json::to_string(&crd).unwrap();
        assert!(json.contains("x-kubernetes-list-type"));
        assert!(json.contains("x-kubernetes-list-map-keys"));
        assert!(json.contains("Value is immutable"));
    }
}
