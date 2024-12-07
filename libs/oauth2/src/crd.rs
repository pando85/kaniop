use kaniop_operator::controller::KanidmResource;
use kaniop_operator::crd::KanidmRef;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Groups are a collection of other entities that exist within Kanidm.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmOAuth2Client",
    plural = "kanidmoauth2clients",
    singular = "kanidmoauth2client",
    shortname = "oauth2",
    namespaced,
    status = "KanidmOAuth2ClientStatus",
    doc = r#"The Kanidm OAuth2 client custom resource definition (CRD) defines an OAuth2 client
    integration in Kanidm.

    This resource has to be in the same namespace as the Kanidm cluster."#,
    printcolumn = r#"{"name":"Exists","type":"string","jsonPath":".status.conditions[?(@.type == 'Exists')].status"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmOAuth2ClientSpec {
    pub kanidm_ref: KanidmRef,

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
    // TODO: move from ValidatingAdmissionPolicy to here when schemars 1.0.0 is released and k8s-openapi implements it
    // schemars = 1.0.0
    //#[schemars(extend("x-kubernetes-validations" = [{"message": "Value is immutable", "rule": "self == oldSelf"}]))]
    #[serde(default)]
    pub public: bool,

    /// Enable strict validation of redirect URLs. Previously redirect URLs only validated the
    /// origin of the URL matched. When enabled, redirect URLs must match exactly.
    ///
    /// Enabled by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_redirect_uri: Option<bool>,

    /// Disable PKCE on this oauth2 client to work around insecure clients that may not support it.
    /// You should request the client to enable PKCE!
    ///
    /// Public clients cannot disable PKCE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_insecure_client_disable_pkce: Option<bool>,

    /// Use the 'name' attribute instead of 'spn' for the preferred_username
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_short_username: Option<bool>,

    /// Allow public clients to redirect to localhost.
    ///
    /// Just public clients can allow localhost redirect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_localhost_redirect: Option<bool>,

    /// Enable legacy signing crypto on this oauth2 client. This defaults to being disabled.
    /// You only need to enable this for openid clients that do not support modern cryptographic
    /// operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_legacy_crypto_enable: Option<bool>,
}

impl KanidmResource for KanidmOAuth2Client {
    #[inline]
    fn kanidm_name(&self) -> String {
        self.spec.kanidm_ref.name.clone()
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
}
