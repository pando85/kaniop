use kaniop_k8s_util::types::normalize_spn;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::{KanidmRef, SecretRotation};
use kaniop_operator::kanidm::crd::Kanidm;

use std::{
    collections::{BTreeSet, HashMap},
    ops::Not,
};

use kanidm_proto::internal::Oauth2ClaimMapJoin;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector};
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The KanidmOAuth2Client custom resource definition (CRD) defines an OAuth2 client integration in
/// Kanidm. This resource allows you to configure OAuth2 clients that can interact with the Kanidm
/// authorization server. The CRD supports various configurations, including scope maps,
/// claim maps, and other OAuth2 client settings.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmOAuth2Client",
    plural = "kanidmoauth2clients",
    singular = "kanidmoauth2client",
    shortname = "oauth2",
    namespaced,
    status = "KanidmOAuth2ClientStatus",
    doc = r#"The Kanidm OAuth2 client custom resource definition (CRD) defines an OAuth2 client
    integration in Kanidm."#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".status.kanidmRef"}"#,
    printcolumn = r#"{"name":"Public","type":"string","jsonPath":".spec.public"}"#,
    printcolumn = r#"{"name":"Secret","type":"string","jsonPath":".status.secretName"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmOAuth2ClientSpec {
    pub kanidm_ref: KanidmRef,

    /// The name of the entity in Kanidm. If not specified, the Kubernetes resource name is used.
    /// Use this field to manage Kanidm entities with names that don't conform to Kubernetes naming rules
    /// (e.g., entities with underscores like `idm_admin` or `idm_all_persons`).
    /// This field is immutable and cannot be changed after creation.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "kanidmName cannot be changed.", "rule": "self == oldSelf"}]))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_name: Option<String>,

    /// Set the display name for the OAuth2 client.
    pub displayname: String,

    /// Set the landing page (home page) of the client. The landing page is where users will be
    /// redirected to from the Kanidm application portal.
    pub origin: String,

    /// Set the URL where the application expects OAuth2 requests to be sent.
    pub redirect_url: Vec<String>,

    /// Create a new OAuth2 public client that requires PKCE. You should prefer using confidential
    /// client types if possible over public ones.
    ///
    /// Public clients have many limitations and can not access all API's of OAuth2. For example
    /// rfc7662 token introspection requires client authentication.
    ///
    /// This cannot be changed after creation. Default value is false.
    /// This field is immutable.
    /// If public is false, the operator will create a secret containing the client secret with
    /// the name `<oauth2-client-name>-kanidm-oauth2-credentials` in the same namespace as the OAuth2 client.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "Public cannot be changed.", "rule": "self == oldSelf"}]))]
    #[serde(default)]
    pub public: bool,

    /// Main scope map for the OAuth2 client. For an authorization to proceed, all scopes requested
    /// by the client must be available in the final scope set that is granted to the account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_map: Option<BTreeSet<KanidmScopeMap>>,

    /// Supplementary scope maps for the OAuth2 client. These function the same as scope maps where
    /// membership of a group provides a set of scopes to the account.
    /// However these scopes are NOT consulted during authorization decisions made by Kanidm.
    /// These scopes exist to allow optional properties to be provided (such as personal
    /// information about a subset of accounts to be revealed) or so that the service may make its
    /// own authorization decisions based on the provided scopes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sup_scope_map: Option<BTreeSet<KanidmScopeMap>>,

    /// Mapping from a group to a custom claims that it provides to members.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_map: Option<BTreeSet<KanidmClaimMap>>,

    /// Enable strict validation of redirect URLs. Previously redirect URLs only validated the
    /// origin of the URL matched. When enabled, redirect URLs must match exactly.
    ///
    /// Enabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_redirect_url: Option<bool>,

    /// Use the 'name' attribute instead of 'spn' for the preferred_username.
    ///
    /// Disabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_short_username: Option<bool>,

    /// Allow public clients to redirect to localhost.
    ///
    /// Just public clients can allow localhost redirect.
    /// Disabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_localhost_redirect: Option<bool>,

    /// Disable PKCE on this oauth2 client to work around insecure clients that may not support it.
    /// You should request the client to enable PKCE!
    ///
    /// Public clients cannot disable PKCE.
    /// PKCE is enabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_insecure_client_disable_pkce: Option<bool>,

    /// Enable legacy signing crypto on this oauth2 client. This defaults to being disabled.
    /// You only need to enable this for openid clients that do not support modern cryptographic
    /// operations.
    ///
    /// Disabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_legacy_crypto_enable: Option<bool>,

    /// Automatic rotation configuration for the OAuth2 client secret. Only applies to confidential
    /// clients (public: false). When enabled, the operator will regenerate the client secret
    /// periodically based on the configured rotation period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_rotation: Option<SecretRotation>,

    /// Optional URL to an image for the OAuth2 client application.
    /// The image will be downloaded and set in Kanidm for display in the application portal.
    /// Constraints:
    /// - Maximum size: 256 KB
    /// - Maximum dimensions: 1024 x 1024 pixels
    /// - Supported formats: png, jpg, gif, svg, webp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<OAuth2ClientImageSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OAuth2ClientImageSpec {
    /// URL to fetch the image from (HTTP/HTTPS only).
    /// The operator will periodically check this URL for changes using HEAD requests
    /// and re-download the image when changes are detected.
    pub url: String,
}

impl KanidmResource for KanidmOAuth2Client {
    #[inline]
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    #[inline]
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector> {
        &kanidm.spec.oauth2_client_namespace_selector
    }

    #[inline]
    fn kanidm_name_override(&self) -> Option<&str> {
        self.spec.kanidm_name.as_deref()
    }
}

/// The `KanidmScopeMap` struct represents a mapping of a group to a set of OAuth2 scopes in Kanidm.
///
/// Scope maps in Kanidm are used to define the permissions that a client application can request on
/// behalf of a user. These scopes determine what resources the client can access and what
/// operations it can perform.
///
/// These provide a set of scopes if a user is a member of a specific group within Kanidm. This
/// allows you to create a relationship between the scopes of a service, and the groups/roles in
/// Kanidm which can be specific to that service.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, PartialOrd, Eq, Ord)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmScopeMap {
    /// Group name or SPN. Members of this group will be granted the scopes defined in the `scopes` field.
    pub group: String,

    /// A scope is a string that represents a specific permission or set of permissions that a
    /// client application can request from an authorization server. Scopes define the level of
    /// access that the client application is granted to the user's resources.
    ///
    /// OpenID Connect allows a number of scopes that affect the content of the resulting authorization
    /// token. If one of the following scopes is requested by the OpenID client, then the associated
    /// claims may be added to the authorization token. It is not guaranteed that all of the associated
    /// claims will be added.
    ///
    /// - `profile`: name, family_name, given_name, middle_name, nickname, preferred_username, profile,
    ///    picture, website, gender, birthdate, zoneinfo, locale, and updated_at
    /// - `email`: email, email_verified
    /// - `address`: address
    /// - `phone`: phone_number, phone_number_verified
    /// - `groups`: groups
    ///
    /// If you are creating an OpenID Connect (OIDC) client, you MUST provide a scope map containing `openid`.
    /// Without this, OpenID Connect clients WILL NOT WORK!
    #[allow(clippy::doc_overindented_list_items)]
    pub scopes: Vec<String>,
}

impl KanidmScopeMap {
    pub fn normalize(self) -> Self {
        let mut scopes = self.scopes;
        scopes.sort();
        Self {
            group: normalize_spn(&self.group),
            scopes,
        }
    }
    pub fn from(s: &str) -> Option<Self> {
        let parts = s.split(':').collect::<Vec<_>>();
        if parts.len() != 2 {
            return None;
        }
        let group = parts[0].to_string();
        let proto_scopes = parts[1];
        if proto_scopes.starts_with(" {").not() || proto_scopes.ends_with('}').not() {
            return None;
        }

        let scopes = proto_scopes[2..proto_scopes.len() - 1]
            .split(", ")
            .map(|s| s.replace('"', ""))
            .collect();
        Some(Self { group, scopes })
    }
}

/// Some OAuth2 services may consume custom claims from an id token for access control or other
/// policy decisions. Each custom claim is a key:values set, where there can be many values
/// associated to a claim name. Different applications may expect these values to be formatted
/// (joined) in different ways.
///
/// Claim values are mapped based on membership to groups. When an account is a member of multiple
/// groups that would receive the same claim, the values of these maps are merged.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, PartialOrd, Eq, Ord)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmClaimMap {
    /// The name of the claim that will be provided to the client.
    pub name: String,

    pub values_map: BTreeSet<KanidmClaimsValuesMap>,

    /// The strategy to join the values together.
    /// Possible strategies to join the values of a claim map:
    /// `csv` -> "value_a,value_b"
    /// `ssv` -> "value_a value_b"
    /// `array` -> ["value_a", "value_b"]
    #[serde(default)]
    pub join_strategy: KanidmClaimMapJoinStrategy,
}

impl KanidmClaimMap {
    pub fn normalize(self) -> Self {
        Self {
            values_map: self
                .values_map
                .iter()
                .map(|vm| vm.clone().normalize())
                .collect(),
            ..self
        }
    }

    pub fn from(s: &str) -> Option<Self> {
        let parts = s.split(':').collect::<Vec<_>>();
        if parts.len() != 4 {
            return None;
        }
        let name = parts[0].to_string();
        let group = parts[1].to_string();
        let join_strategy = match parts[2] {
            "," => KanidmClaimMapJoinStrategy::Csv,
            " " => KanidmClaimMapJoinStrategy::Ssv,
            ";" => KanidmClaimMapJoinStrategy::Array,
            _ => return None,
        };
        let proto_values = parts[3];
        if proto_values.starts_with('"').not() || proto_values.ends_with('"').not() {
            return None;
        }
        let values = proto_values[1..proto_values.len() - 1]
            .split(",")
            .map(|s| s.to_string())
            .collect();
        Some(Self {
            name,
            join_strategy,
            values_map: BTreeSet::from([KanidmClaimsValuesMap { group, values }]),
        })
    }

    pub fn group(v: &[KanidmClaimMap]) -> BTreeSet<KanidmClaimMap> {
        let mut map: HashMap<String, KanidmClaimMap> = HashMap::new();

        for claim in v {
            if let Some(existing) = map.get_mut(&claim.name) {
                existing.values_map.extend(claim.values_map.clone());
            } else {
                map.insert(claim.name.clone(), claim.clone());
            }
        }

        map.into_values().collect()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, PartialOrd, Eq, Ord)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmClaimsValuesMap {
    /// The group name or SPN that will provide the claim.
    pub group: String,

    /// The values that will be provided to the client.
    pub values: Vec<String>,
}

impl KanidmClaimsValuesMap {
    pub fn normalize(self) -> Self {
        let mut values = self.values;
        values.sort();
        Self {
            group: normalize_spn(&self.group),
            values,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, PartialOrd, Eq, Ord)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum KanidmClaimMapJoinStrategy {
    /// Comma-separated values: "value_a,value_b"
    Csv,
    /// Space-separated values: "value_a value_b"
    Ssv,
    /// Array format: ["value_a", "value_b"]
    #[default]
    Array,
}

impl KanidmClaimMapJoinStrategy {
    pub fn to_oauth2_claim_map_join(&self) -> Oauth2ClaimMapJoin {
        match self {
            KanidmClaimMapJoinStrategy::Csv => Oauth2ClaimMapJoin::Csv,
            KanidmClaimMapJoinStrategy::Ssv => Oauth2ClaimMapJoin::Ssv,
            KanidmClaimMapJoinStrategy::Array => Oauth2ClaimMapJoin::Array,
        }
    }
}

/// Most recent observed status of the Kanidm Group. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmOAuth2ClientStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_map: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sup_scope_map: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub claims_map: Option<Vec<String>>,

    pub ready: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,

    pub kanidm_ref: String,

    /// Status of the OAuth2 client image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<OAuth2ClientImageStatus>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OAuth2ClientImageStatus {
    /// The URL from which the image was last fetched.
    pub url: String,
    /// ETag header from the last fetch (for change detection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Last-Modified header from the last fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Content-Length from the last fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    /// Hash of the image content (SHA-256, for validation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        KanidmClaimMap, KanidmClaimMapJoinStrategy, KanidmClaimsValuesMap, KanidmScopeMap,
    };

    use std::collections::BTreeSet;

    #[test]
    fn test_kanidm_scope_map_from() {
        let scope_map = KanidmScopeMap {
            group: "test_group".to_string(),
            scopes: vec!["scope1".to_string(), "scope2".to_string()],
        };

        assert_eq!(
            KanidmScopeMap::from(r#"test_group@test.example.com: {"scope1", "scope2"}"#)
                .unwrap()
                .normalize(),
            scope_map,
        );

        assert_eq!(
            KanidmScopeMap::from(r#"test_group: {"scope1", "scope2"}"#)
                .unwrap()
                .normalize(),
            scope_map,
        );

        assert!(KanidmScopeMap::from(r#"test_group:{"scope1", "scope2"}"#).is_none(),);
        assert_eq!(
            KanidmScopeMap::from(r#"test_group: {"scope1"}"#)
                .unwrap()
                .normalize(),
            KanidmScopeMap {
                group: "test_group".to_string(),
                scopes: vec!["scope1".to_string()],
            }
        );
    }

    #[test]
    fn test_group_kanidm_claims_map() {
        let claim1 = KanidmClaimMap {
            name: "claim1".to_string(),
            values_map: BTreeSet::from([KanidmClaimsValuesMap {
                group: "group1".to_string(),
                values: vec!["value1".to_string()],
            }]),
            join_strategy: KanidmClaimMapJoinStrategy::Array,
        };

        let claim2 = KanidmClaimMap {
            name: "claim1".to_string(),
            values_map: BTreeSet::from([KanidmClaimsValuesMap {
                group: "group2".to_string(),
                values: vec!["value2".to_string()],
            }]),
            join_strategy: KanidmClaimMapJoinStrategy::Array,
        };

        let claim3 = KanidmClaimMap {
            name: "claim2".to_string(),
            values_map: BTreeSet::from([KanidmClaimsValuesMap {
                group: "group3".to_string(),
                values: vec!["value3".to_string()],
            }]),
            join_strategy: KanidmClaimMapJoinStrategy::Array,
        };

        let claims = vec![claim1.clone(), claim2.clone(), claim3.clone()];

        let grouped_claims = KanidmClaimMap::group(&claims);

        assert_eq!(grouped_claims.len(), 2);

        let grouped_claim1 = grouped_claims.iter().find(|c| c.name == "claim1").unwrap();
        assert_eq!(grouped_claim1.values_map.len(), 2);
        assert!(grouped_claim1.values_map.contains(&KanidmClaimsValuesMap {
            group: "group1".to_string(),
            values: vec!["value1".to_string()],
        }));
        assert!(grouped_claim1.values_map.contains(&KanidmClaimsValuesMap {
            group: "group2".to_string(),
            values: vec!["value2".to_string()],
        }));

        let grouped_claim2 = grouped_claims.iter().find(|c| c.name == "claim2").unwrap();
        assert_eq!(grouped_claim2.values_map.len(), 1);
        assert!(grouped_claim2.values_map.contains(&KanidmClaimsValuesMap {
            group: "group3".to_string(),
            values: vec!["value3".to_string()],
        }));
    }

    #[test]
    fn test_kanidm_claim_map_from() {
        let mut claim_map = KanidmClaimMap {
            name: "claim_name".to_string(),
            join_strategy: KanidmClaimMapJoinStrategy::Array,
            values_map: BTreeSet::from([KanidmClaimsValuesMap {
                group: "group_name".to_string(),
                // Sorted values after normalization
                values: vec!["boo".to_string(), "foo".to_string()],
            }]),
        };

        assert_eq!(
            KanidmClaimMap::from(r#"claim_name:group_name@test.example.com:;:"foo,boo""#)
                .unwrap()
                .normalize(),
            claim_map,
        );
        assert_eq!(
            KanidmClaimMap::from(r#"claim_name:group_name:;:"foo,boo""#)
                .unwrap()
                .normalize(),
            claim_map,
        );

        claim_map.join_strategy = KanidmClaimMapJoinStrategy::Csv;
        assert_eq!(
            KanidmClaimMap::from(r#"claim_name:group_name:,:"foo,boo""#)
                .unwrap()
                .normalize(),
            claim_map,
        );

        claim_map.join_strategy = KanidmClaimMapJoinStrategy::Ssv;
        assert_eq!(
            KanidmClaimMap::from(r#"claim_name:group_name: :"foo,boo""#)
                .unwrap()
                .normalize(),
            claim_map,
        );
    }
}
