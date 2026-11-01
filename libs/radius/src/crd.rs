//! Proposed public API for independently managed Kanidm RADIUS endpoints.
//!
//! This is the *source* of the CRD. Generated Kubernetes CRD YAML and example
//! manifests must be produced by the repository's Rust generators, never
//! hand-edited. Runtime reconciliation and registration are separate gates.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Affinity, ResourceRequirements, Toleration, TopologySpreadConstraint,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector, Time};
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::{KanidmRef, SecretRotation};
use kaniop_operator::kanidm::crd::Kanidm;
use serde::{Deserialize, Serialize};

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
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".status.endpoint"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    validation = Rule::new("size(self.metadata.name) <= 52")
        .message("RADIUS server name must be at most 52 characters"),
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
    // max is defined so the API server can estimate CEL expression costs; it matches the rule.
    #[schemars(length(max = 100))]
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

    /// Periodic API token rotation (disabled unless explicitly enabled). When
    /// enabled, the token is replaced after `periodDays` (default 90, accepted
    /// range 1 to 365) and new tokens expire after twice that period.
    #[serde(default)]
    pub token_rotation: SecretRotation,

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
    // max is defined so the API server can estimate CEL expression costs; it matches the rule.
    // Items are Kanidm group SPNs (name@domain), bounded by the DNS name limit.
    #[schemars(length(max = 100), inner(length(max = 253)))]
    pub required_groups: Vec<String>,

    /// Fallback VLAN for permitted users. Valid production VLAN range: 1..4094.
    #[serde(default = "default_vlan")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS default VLAN must be in 1..4094", "rule": "self >= 1 && self <= 4094"}]))]
    pub default_vlan: i32,

    /// Optional per-group VLAN mappings, independent of authorization groups.
    // max is defined so the API server can estimate CEL expression costs of item rules.
    #[schemars(length(max = 100))]
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
    #[schemars(length(max = 253))]
    pub group: String,
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS VLAN must be in 1..4094", "rule": "self >= 1 && self <= 4094"}]))]
    pub vlan: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusClientSpec {
    /// FreeRADIUS client name; safe ASCII letters, numbers, dash, underscore.
    // max matches the 63 character limit in the rule below, bounding its estimated cost.
    #[schemars(length(max = 63))]
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

/// Same-namespace reference to a PEM CA bundle. ConfigMap is the default
/// for non-secret trust anchors; Secret supports privately managed CA chains.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadiusCaBundleRef {
    /// Source type: ConfigMap (default) or Secret.
    #[serde(default)]
    pub kind: RadiusCaKind,
    /// Name of the ConfigMap or Secret containing the CA bundle.
    pub name: String,
    /// Key containing the PEM-encoded CA certificates.
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
#[cfg_attr(feature = "schemars", schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS externalTrafficPolicy is only allowed for NodePort and LoadBalancer Services", "rule": "(has(self.type) ? self.type : 'ClusterIP') != 'ClusterIP' || !has(self.externalTrafficPolicy)"}])))]
pub struct RadiusServiceSpec {
    /// ClusterIP, NodePort or LoadBalancer. Defaults to ClusterIP.
    #[serde(rename = "type", default = "default_service_type")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS Service must be ClusterIP, NodePort or LoadBalancer", "rule": "self in ['ClusterIP','NodePort','LoadBalancer']"}]))]
    pub type_: String,

    /// Exposed UDP Service port, defaults to 1812.
    #[serde(default = "default_service_port")]
    #[schemars(extend("x-kubernetes-validations" = [{"message": "RADIUS UDP port must be 1..65535", "rule": "self >= 1 && self <= 65535"}]))]
    pub port: i32,

    /// Cluster or Local. Only allowed for NodePort and LoadBalancer Services:
    /// the CRD rejects it on a ClusterIP Service, as Kubernetes does.
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

/// Observed state, recomputed from Kanidm and Kubernetes on every reconcile.
///
/// Status is a report and cache only: the controller never reads it to decide
/// what to do, so losing it changes nothing. It carries token **ids** only,
/// never token values or NAS secrets.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRadiusServerStatus {
    /// `IdentityReady`, `ConfigurationReady`, `Available`, `Exposed` and
    /// `Rotating`; all five are always present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Option<Vec<Condition>>,

    /// True when the identity and configuration are ready, the rollout is
    /// complete and available, and the Service is exposed.
    #[serde(default)]
    pub ready: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Address clients should use, `host:port` of the Service (shown as the
    /// `Endpoint` print column): the cluster IP for `ClusterIP`, `*:<nodePort>`
    /// (any node address) for `NodePort`, and the load balancer address for
    /// `LoadBalancer`. Absent until an address is assigned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Id of the Kanidm API token the Deployment template runs with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,

    /// When Kanidm issued the current token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_issued_at: Option<Time>,

    /// Immutable config Secret the Deployment template mounts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_secret_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,

    /// Desired replicas of the Deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_replicas: Option<i32>,

    /// Tokens still on the account but no longer current; revoked once no
    /// pod, ReplicaSet or template uses them. Informational only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retiring_token_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_explicit_and_are_not_permissive() {
        assert_eq!(RadiusServiceSpec::default().type_, "ClusterIP");
        assert_eq!(RadiusServiceSpec::default().port, 1812);
        assert!(!SecretRotation::default().enabled);
        assert_eq!(SecretRotation::default().period_days, 90);
        let spec: KanidmRadiusServerSpec = serde_json::from_value(serde_json::json!({
            "kanidmRef": {"name": "idm"},
            "authorization": {"requiredGroups": ["allowed@idm.example.com"]},
            "clients": [{"name": "nas", "ipaddr": "192.0.2.1", "sharedSecretRef": {"name": "nas-secret", "key": "nas"}}],
            "radiusTls": {"secretRef": {"name": "eap-tls"}}
        })).unwrap();
        assert_eq!(spec.token_rotation.period_days, 90);
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
        // Explicitly serializing the default makes generated examples show
        // the CA source kind without changing deserialization compatibility.
        let serialized = serde_json::to_value(&implicit).unwrap();
        assert_eq!(serialized["kind"], "ConfigMap");

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
    fn status_reports_token_ids_and_round_trips() {
        let status = KanidmRadiusServerStatus {
            token_id: Some("9ad74e68-147f-49e8-96db-dc21c920ff3b".into()),
            config_secret_name: Some("campus-0123456789".into()),
            retiring_token_ids: vec!["31b30b65-037a-418f-b7ca-0c376da2db28".into()],
            ..Default::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["tokenId"], "9ad74e68-147f-49e8-96db-dc21c920ff3b");
        assert_eq!(value["configSecretName"], "campus-0123456789");
        // An empty retiring list is omitted.
        let empty = serde_json::to_value(KanidmRadiusServerStatus::default()).unwrap();
        assert!(empty.get("retiringTokenIds").is_none());
        let restored: KanidmRadiusServerStatus = serde_json::from_value(value).unwrap();
        assert_eq!(restored, status);
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
            Some(&serde_json::json!(90))
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

    #[cfg(feature = "schemars")]
    #[test]
    fn cel_validated_lists_and_strings_are_bounded() {
        use kube::CustomResourceExt;

        // Unbounded lists/strings make the API server reject the CRD because the
        // estimated CEL rule cost exceeds its budget.
        let schema = serde_json::to_value(KanidmRadiusServer::crd()).unwrap();
        let spec = "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties";
        let at = |path: &str| schema.pointer(&format!("{spec}/{path}")).cloned();

        for list in [
            "clients",
            "authorization/properties/requiredGroups",
            "authorization/properties/groupVlans",
        ] {
            assert_eq!(
                at(&format!("{list}/maxItems")),
                Some(serde_json::json!(100)),
                "{list} must define maxItems"
            );
        }
        assert_eq!(
            at("clients/items/properties/name/maxLength"),
            Some(serde_json::json!(63))
        );
        assert_eq!(
            at("authorization/properties/requiredGroups/items/maxLength"),
            Some(serde_json::json!(253))
        );
        assert_eq!(
            at("authorization/properties/groupVlans/items/properties/group/maxLength"),
            Some(serde_json::json!(253))
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn name_length_rule_matches_the_generated_name_limit() {
        use kube::CustomResourceExt;

        let value = serde_json::to_value(KanidmRadiusServer::crd()).unwrap();
        let rules = value
            .pointer("/spec/versions/0/schema/openAPIV3Schema/x-kubernetes-validations")
            .and_then(|rules| rules.as_array())
            .expect("root x-kubernetes-validations");
        let max = crate::reconcile::workload::MAX_RADIUS_NAME_LEN;
        assert!(
            rules
                .iter()
                .any(|rule| rule["rule"] == format!("size(self.metadata.name) <= {max}")),
            "{rules:?}"
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn service_rule_rejects_external_traffic_policy_on_cluster_ip() {
        use kube::CustomResourceExt;

        let value = serde_json::to_value(KanidmRadiusServer::crd()).unwrap();
        let service = value
            .pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/service")
            .expect("service schema");
        let rules = service["x-kubernetes-validations"]
            .as_array()
            .expect("service x-kubernetes-validations");
        // `type` defaults to ClusterIP, so the rule must hold when it is absent.
        assert!(
            rules.iter().any(|rule| rule["rule"]
                == "(has(self.type) ? self.type : 'ClusterIP') != 'ClusterIP' || !has(self.externalTrafficPolicy)"
                && rule["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("only allowed for NodePort and LoadBalancer"))),
            "{rules:?}"
        );
        assert_eq!(service["properties"]["type"]["default"], "ClusterIP");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn print_columns_show_readiness_and_endpoint() {
        use kube::CustomResourceExt;

        let value = serde_json::to_value(KanidmRadiusServer::crd()).unwrap();
        let columns = value
            .pointer("/spec/versions/0/additionalPrinterColumns")
            .and_then(|columns| columns.as_array())
            .unwrap();
        let names: Vec<_> = columns
            .iter()
            .map(|column| column["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Kanidm", "Ready", "Endpoint", "Age"]);
        let status = KanidmRadiusServerStatus {
            endpoint: Some("10.0.0.1:1812".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(status).unwrap()["endpoint"],
            "10.0.0.1:1812"
        );
    }
}
