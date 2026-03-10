use kanidm_proto::{
    constants::{
        ATTR_ALLOW_PRIMARY_CRED_FALLBACK, ATTR_AUTH_PASSWORD_MINIMUM_LENGTH,
        ATTR_AUTH_SESSION_EXPIRY, ATTR_CREDENTIAL_TYPE_MINIMUM, ATTR_GIDNUMBER,
        ATTR_LIMIT_SEARCH_MAX_FILTER_TEST, ATTR_LIMIT_SEARCH_MAX_RESULTS, ATTR_PRIVILEGE_EXPIRY,
        ATTR_WEBAUTHN_ATTESTATION_CA_LIST, ENTRYCLASS_ACCOUNT_POLICY,
    },
    v1::Entry,
};
use kaniop_k8s_util::types::get_first_cloned;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::KanidmRef;
use kaniop_operator::kanidm::crd::Kanidm;

use std::fmt;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector};
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
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmGroup",
    plural = "kanidmgroups",
    singular = "kanidmgroup",
    shortname = "kg",
    namespaced,
    status = "KanidmGroupStatus",
    doc = r#"The Kanidm group custom resource definition (CRD) defines a group in Kanidm."#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".status.kanidmRef"}"#,
    printcolumn = r#"{"name":"ManagedBy","type":"string","jsonPath":".spec.entryManagedBy"}"#,
    printcolumn = r#"{"name":"GID","type":"integer","jsonPath":".status.gid"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmGroupSpec {
    pub kanidm_ref: KanidmRef,

    /// The name of the entity in Kanidm. If not specified, the Kubernetes resource name is used.
    /// Use this field to manage Kanidm entities with names that don't conform to Kubernetes naming rules
    /// (e.g., entities with underscores like `idm_admin` or `idm_all_persons`).
    /// This field is immutable and cannot be changed after creation.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "kanidmName cannot be changed.", "rule": "self == oldSelf"}]))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_name: Option<String>,

    /// Optional name/spn of a group or account that have entry manager rights over this group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_managed_by: Option<String>,

    /// Set the exact list of mail addresses that this group is associated with. The first mail
    /// address in the list is the `primary` and the remainder are aliases. Setting an empty list
    /// will clear the mail attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<Vec<String>>,

    /// Name or SPN of group members. Set the exact list of members that this group should contain,
    /// removing any not listed in the set operation.
    /// If you want to manage members from the database, do not set them here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,

    /// POSIX attributes for the group account. When specified, the operator will activate them.
    /// If omitted, the operator retains the attributes in the database but ceases to manage them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posix_attributes: Option<KanidmGroupPosixAttributes>,

    /// Account policy settings for the group.
    ///
    /// When set, the operator will enable account policy on this group and configure the specified
    /// settings. Account policy defines security requirements that accounts must meet when they are
    /// members of this group.
    ///
    /// When an account is affected by multiple policies, the strictest component from each policy
    /// is applied.
    ///
    /// More info: https://kanidm.github.io/kanidm/stable/accounts/account_policy.html
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_policy: Option<KanidmGroupAccountPolicy>,
}

impl KanidmResource for KanidmGroup {
    #[inline]
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    #[inline]
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector> {
        &kanidm.spec.group_namespace_selector
    }

    #[inline]
    fn kanidm_name_override(&self) -> Option<&str> {
        self.spec.kanidm_name.as_deref()
    }
}

/// Kanidm has features that enable its accounts and groups to be consumed on POSIX-like machines,
/// such as Linux, FreeBSD or others. Both service accounts and person accounts can be used on POSIX
/// systems.
///
/// The attributes defined here are set by the operator. If you want to manage those attributes
/// from the database, do not set them here.
/// Additionally, if you unset them here, they will be kept in the database.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmGroupPosixAttributes {
    /// The group ID number (GID) for the group account.
    ///
    /// If omitted, Kanidm will generate it automatically.
    pub gidnumber: Option<u32>,
}

impl PartialEq for KanidmGroupPosixAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        self.gidnumber.is_none() || self.gidnumber == other.gidnumber
    }
}

impl From<Entry> for KanidmGroupPosixAttributes {
    fn from(entry: Entry) -> Self {
        KanidmGroupPosixAttributes {
            gidnumber: get_first_cloned(&entry, ATTR_GIDNUMBER).and_then(|s| s.parse::<u32>().ok()),
        }
    }
}

/// Minimum security strength of credentials that may be assigned to accounts affected by this
/// policy. In order from weakest to strongest: any < mfa < passkey < attested_passkey.
///
/// More info:
/// https://kanidm.github.io/kanidm/stable/accounts/account_policy.html#credential-type-minimum
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CredentialTypeMinimum {
    /// Any credential type is allowed (weakest)
    #[default]
    Any,
    /// Multi-factor authentication required
    Mfa,
    /// Passkey (WebAuthn) required
    Passkey,
    /// Attested passkey required (strongest) - requires configuring WebAuthn attestation CA list
    AttestedPasskey,
}

impl fmt::Display for CredentialTypeMinimum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialTypeMinimum::Any => write!(f, "any"),
            CredentialTypeMinimum::Mfa => write!(f, "mfa"),
            CredentialTypeMinimum::Passkey => write!(f, "passkey"),
            CredentialTypeMinimum::AttestedPasskey => write!(f, "attested_passkey"),
        }
    }
}

impl CredentialTypeMinimum {
    /// Parse from Kanidm string value
    pub fn from_kanidm_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "any" => Some(CredentialTypeMinimum::Any),
            "mfa" => Some(CredentialTypeMinimum::Mfa),
            "passkey" => Some(CredentialTypeMinimum::Passkey),
            "attested_passkey" => Some(CredentialTypeMinimum::AttestedPasskey),
            _ => None,
        }
    }
}

/// Custom schema for CredentialTypeMinimum that is compatible with Kubernetes CRD structural schema.
/// Kubernetes doesn't support `anyOf` with nullable, so we generate a flat enum schema with nullable.
/// We use `x-enum-descriptions` to provide per-option descriptions for documentation generation.
#[cfg(feature = "schemars")]
fn credential_type_minimum_schema(
    _generator: &mut schemars::generate::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "description": "Minimum security strength of credentials that may be assigned to accounts affected by this policy. In order from weakest to strongest: any < mfa < passkey < attested_passkey.\n\nMore info: https://kanidm.github.io/kanidm/stable/accounts/account_policy.html#credential-type-minimum",
        "type": "string",
        "enum": ["any", "mfa", "passkey", "attested_passkey"],
        "x-enum-descriptions": [
            "Any credential type is allowed (weakest)",
            "Multi-factor authentication required",
            "Passkey (WebAuthn) required",
            "Attested passkey required (strongest) - requires configuring WebAuthn attestation CA list"
        ],
        "nullable": true
    })
}

/// Account policy settings for the group.
///
/// Account Policy defines the security requirements that accounts must meet and influences users
/// sessions. Policy is defined on groups so that membership of a group influences the security of
/// its members. This allows you to express that if you can access a system or resource, then the
/// account must also meet the policy requirements.
///
/// When an account is affected by multiple policies, the strictest component from each policy is
/// applied.
///
/// More info: https://kanidm.github.io/kanidm/stable/accounts/account_policy.html
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmGroupAccountPolicy {
    /// Maximum length in seconds that an authentication session may exist for.
    /// After this time, the user must reauthenticate.
    ///
    /// This value provides a difficult balance - forcing frequent re-authentications can frustrate
    /// and annoy users. However extremely long sessions allow a stolen or disclosed session
    /// token/device to read data for an extended period.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_session_expiry: Option<u32>,

    /// Minimum security strength of credentials that may be assigned to accounts affected by this
    /// policy. In order from weakest to strongest: any < mfa < passkey < attested_passkey.
    ///
    /// `attested_passkey` requires configuring `webauthnAttestationCaList`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "credential_type_minimum_schema", default)
    )]
    pub credential_type_minimum: Option<CredentialTypeMinimum>,

    /// Minimum length for passwords (if they are allowed by credential_type_minimum).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_minimum_length: Option<u32>,

    /// Maximum length in seconds that privileges will exist after reauthentication for a read/write
    /// session. After this time, the session returns to read-only mode.
    ///
    /// Maximum allowed value is 3600 (1 hour).
    #[cfg_attr(feature = "schemars", schemars(extend("x-kubernetes-validations" = [
        {
            "message": "privilegeExpiry must be at most 3600 seconds (1 hour)",
            "rule": "self <= 3600"
        }
    ])))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privilege_expiry: Option<u32>,

    /// WebAuthn attestation CA list. This is the list of certificate authorities and device AAGUIDs
    /// that must be used by members of this policy. This allows limiting devices to specific models.
    ///
    /// Generate this list using `fido-mds-tool` from the webauthn-rs project.
    ///
    /// More info:
    /// https://kanidm.github.io/kanidm/stable/accounts/account_policy.html#setting-webauthn-attestation-ca-lists
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webauthn_attestation_ca_list: Option<String>,

    /// Allow authenticating with the primary account password when logging in via LDAP.
    /// If both an LDAP and primary password are specified, Kanidm will only accept the LDAP
    /// password.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_primary_cred_fallback: Option<bool>,

    /// Maximum number of results returned from a search operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_search_max_results: Option<u32>,

    /// Maximum number of filter tests allowed in a search operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_search_max_filter_test: Option<u32>,
}

/// Account policy attributes as read from Kanidm. Used to compare desired vs current state.
#[derive(Clone, Debug, Default)]
pub struct KanidmGroupAccountPolicyAttributes {
    /// Whether account policy is enabled on this group
    pub enabled: bool,
    pub auth_session_expiry: Option<u32>,
    pub credential_type_minimum: Option<CredentialTypeMinimum>,
    pub password_minimum_length: Option<u32>,
    pub privilege_expiry: Option<u32>,
    pub webauthn_attestation_ca_list: Option<String>,
    pub allow_primary_cred_fallback: Option<bool>,
    pub limit_search_max_results: Option<u32>,
    pub limit_search_max_filter_test: Option<u32>,
}

impl From<&Entry> for KanidmGroupAccountPolicyAttributes {
    fn from(entry: &Entry) -> Self {
        let enabled = entry
            .attrs
            .get("class")
            .is_some_and(|classes| classes.iter().any(|c| c == ENTRYCLASS_ACCOUNT_POLICY));

        KanidmGroupAccountPolicyAttributes {
            enabled,
            auth_session_expiry: get_first_cloned(entry, ATTR_AUTH_SESSION_EXPIRY)
                .and_then(|s| s.parse::<u32>().ok()),
            credential_type_minimum: get_first_cloned(entry, ATTR_CREDENTIAL_TYPE_MINIMUM)
                .and_then(|s| CredentialTypeMinimum::from_kanidm_str(&s)),
            password_minimum_length: get_first_cloned(entry, ATTR_AUTH_PASSWORD_MINIMUM_LENGTH)
                .and_then(|s| s.parse::<u32>().ok()),
            privilege_expiry: get_first_cloned(entry, ATTR_PRIVILEGE_EXPIRY)
                .and_then(|s| s.parse::<u32>().ok()),
            webauthn_attestation_ca_list: get_first_cloned(
                entry,
                ATTR_WEBAUTHN_ATTESTATION_CA_LIST,
            ),
            allow_primary_cred_fallback: get_first_cloned(entry, ATTR_ALLOW_PRIMARY_CRED_FALLBACK)
                .and_then(|s| s.parse::<bool>().ok()),
            limit_search_max_results: get_first_cloned(entry, ATTR_LIMIT_SEARCH_MAX_RESULTS)
                .and_then(|s| s.parse::<u32>().ok()),
            limit_search_max_filter_test: get_first_cloned(
                entry,
                ATTR_LIMIT_SEARCH_MAX_FILTER_TEST,
            )
            .and_then(|s| s.parse::<u32>().ok()),
        }
    }
}

/// Most recent observed status of the Kanidm Group. Read-only.
///
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmGroupStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    pub ready: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,

    pub kanidm_ref: String,
}
