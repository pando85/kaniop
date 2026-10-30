//! Proposed public API for independently managed Kanidm RADIUS endpoints.
//!
//! This is the *source* of the CRD. Generated Kubernetes CRD YAML and example
//! manifests must be produced by the repository's Rust generators, never
//! hand-edited. Runtime reconciliation and registration are separate gates.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Affinity, ResourceRequirements, Toleration, TopologySpreadConstraint,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector};
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::controller::kanidm::KanidmResource;
use crate::crd::KanidmRef;
use crate::kanidm::crd::Kanidm;

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
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS replicas must be between 1 and 100", "rule": "self >= 1 && self <= 100"}]))]
    pub replicas: i32,

    /// Authorization groups and VLAN mapping. At least one required group must
    /// be present to avoid inadvertently permitting all Kanidm users.
    pub authorization: RadiusAuthorizationSpec,

    /// Allowed NAS clients. Never put the RADIUS shared secret in the CRD:
    /// only references to same-namespace Kubernetes Secret keys are accepted.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "At least one RADIUS NAS client is required", "rule": "self.size() > 0 && self.size() <= 100"}]))]
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

impl KanidmResource for KanidmRadiusServer {
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector> {
        &kanidm.spec.radius_server_namespace_selector
    }

    fn kanidm_name_override(&self) -> Option<&str> {
        None
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusAuthorizationSpec {
    /// Users must belong to at least one of these Kanidm groups.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "At least one RADIUS authorization group is required", "rule": "self.size() > 0 && self.size() <= 100"}]))]
    pub required_groups: Vec<String>,

    /// Fallback VLAN for permitted users. Valid production VLAN range: 1..4094.
    #[serde(default = "default_vlan")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS default VLAN must be in 1..4094", "rule": "self >= 1 && self <= 4094"}]))]
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
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS VLAN must be in 1..4094", "rule": "self >= 1 && self <= 4094"}]))]
    pub vlan: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusClientSpec {
    /// FreeRADIUS client name; safe ASCII letters, numbers, dash, underscore.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS NAS client name must be a safe FreeRADIUS identifier", "rule": "self.matches(r'^[A-Za-z0-9_-]{1,63}$')"}]))]
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

/// Which Kubernetes object contains the trusted Kanidm HTTPS CA bundle.
/// Only these two kinds are allowed; no implicit use of arbitrary references.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "PascalCase")]
pub enum RadiusCaKind {
    #[default]
    ConfigMap,
    Secret,
}

fn is_config_map_kind(kind: &RadiusCaKind) -> bool {
    *kind == RadiusCaKind::ConfigMap
}

/// Same-namespace reference to a PEM CA bundle. ConfigMap is the default
/// for non-secret trust anchors; Secret supports privately managed CA chains.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusCaBundleRef {
    #[serde(default, skip_serializing_if = "is_config_map_kind")]
    pub kind: RadiusCaKind,
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
    pub ca_bundle_ref: Option<RadiusCaBundleRef>,
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
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS Service must be ClusterIP, NodePort or LoadBalancer", "rule": "self in ['ClusterIP','NodePort','LoadBalancer']"}]))]
    pub type_: String,

    /// Exposed UDP Service port, defaults to 1812.
    #[serde(default = "default_service_port")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS UDP port must be 1..65535", "rule": "self >= 1 && self <= 65535"}]))]
    pub port: i32,

    /// Cluster or Local, only meaningful for NodePort and LoadBalancer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS externalTrafficPolicy must be Cluster or Local", "rule": "self in ['Cluster','Local']"}]))]
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RadiusTokenRotationSpec {
    /// Opt in to automatic rotation. A new read-only token must be proven
    /// usable before the existing token is revoked.
    #[serde(default)]
    pub enabled: bool,

    /// Rotation interval, in days. Defaults to 30.
    #[serde(default = "default_rotation_days")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS token rotation period must be 1..365 days", "rule": "self >= 1 && self <= 365"}]))]
    pub period_days: i32,
}

fn default_rotation_days() -> i32 {
    30
}

impl Default for RadiusTokenRotationSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            period_days: default_rotation_days(),
        }
    }
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

    /// Persisted per-UID token generation IDs used for restart recovery.
    /// These are non-secret UUIDs, NEVER bearer token values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_token_generation: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_token_generation: Option<String>,

    /// Active and pending immutable config Secret names, which must refer
    /// only to Secrets owned by this resource's immutable Kubernetes UID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_config_secret: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_config_secret: Option<String>,

    /// The actually deployed generation of configuration, not a secret
    /// content digest or a claim of successful RADIUS authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_config_revision: Option<String>,

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
        assert!(!RadiusTokenRotationSpec::default().enabled);
        assert_eq!(RadiusTokenRotationSpec::default().period_days, 30);
        let spec: KanidmRadiusServerSpec = serde_json::from_value(serde_json::json!({
            "kanidmRef": {"name": "idm"},
            "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
            "clients": [{"name": "nas", "ipaddr": "192.0.2.1", "sharedSecretRef": {"name": "nas-secret", "key": "nas"}}],
            "radiusTls": {"secretRef": {"name": "eap-tls"}}
        })).unwrap();
        assert_eq!(spec.token_rotation.period_days, 30);
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

    #[test]
    fn kanidm_ca_ref_kind_defaults_to_config_map_and_supports_secret() {
        let implicit: RadiusCaBundleRef = serde_json::from_value(serde_json::json!({
            "name": "kanidm-ca", "key": "ca.crt"
        }))
        .unwrap();
        assert_eq!(implicit.kind, RadiusCaKind::ConfigMap);
        // Existing generated examples omit the default kind.
        let serialized = serde_json::to_value(&implicit).unwrap();
        assert!(serialized.get("kind").is_none());

        let explicit: RadiusCaBundleRef = serde_json::from_value(serde_json::json!({
            "kind": "Secret", "name": "private-ca", "key": "ca.crt"
        }))
        .unwrap();
        assert_eq!(explicit.kind, RadiusCaKind::Secret);
        assert_eq!(serde_json::to_value(&explicit).unwrap()["kind"], "Secret");
        assert!(
            serde_json::from_value::<RadiusCaBundleRef>(serde_json::json!({
                "kind": "Pod", "name": "bad", "key": "ca.crt"
            }))
            .is_err()
        );
    }

    #[test]
    fn rotation_status_persists_only_token_ids_and_generation_references() {
        let status = KanidmRadiusServerStatus {
            active_token_id: Some("token-uuid-A".into()),
            pending_token_id: Some("token-uuid-B".into()),
            active_token_generation: Some("generation-A".into()),
            pending_token_generation: Some("generation-B".into()),
            active_config_secret: Some("krs-uid-generation-A".into()),
            pending_config_secret: Some("krs-uid-generation-B".into()),
            observed_config_revision: Some("generation-A".into()),
            ..Default::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["pendingTokenGeneration"], "generation-B");
        assert_eq!(value["activeConfigSecret"], "krs-uid-generation-A");
        assert!(value.get("authToken").is_none());
        assert!(value.get("nasSecret").is_none());
        let restored: KanidmRadiusServerStatus = serde_json::from_value(value).unwrap();
        assert_eq!(restored.pending_token_id, status.pending_token_id);
        assert_eq!(restored.pending_config_secret, status.pending_config_secret);
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn conditions_have_map_list_schema_and_refs_are_immutable() {
        use kube::CustomResourceExt;

        let crd = KanidmRadiusServer::crd();
        // Catch a schema default that violates its own CEL validation before
        // Helm/Kind installation (e.g. an accidental derived Rust Default=0).
        let schema = serde_json::to_value(&crd).unwrap();
        assert_eq!(
            schema.pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/tokenRotation/default/periodDays"),
            Some(&serde_json::json!(30))
        );
        let json = serde_json::to_string(&crd).unwrap();
        assert!(json.contains("x-kubernetes-list-type"));
        assert!(json.contains("x-kubernetes-list-map-keys"));
        assert!(json.contains("Value is immutable"));
        assert!(json.contains("At least one RADIUS NAS client is required"));
        assert!(json.contains("At least one RADIUS authorization group is required"));
        assert!(json.contains("RADIUS default VLAN must be in 1..4094"));
        assert!(json.contains("RADIUS NAS client name must be a safe FreeRADIUS identifier"));
    }
}
