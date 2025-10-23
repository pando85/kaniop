use std::collections::BTreeSet;

use k8s_openapi::chrono::DateTime;
use kanidm_proto::internal::{ApiToken, ApiTokenPurpose};
use kaniop_k8s_util::types::{get_first_cloned, normalize_spn, parse_time};
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::{KanidmAccountPosixAttributes, KanidmRef};
use kaniop_operator::kanidm::crd::Kanidm;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector, Time};
use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_DISPLAYNAME, ATTR_ENTRY_MANAGED_BY,
    ATTR_MAIL,
};
use kanidm_proto::v1::Entry;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A service account represents a non-human account in Kanidm used for programmatic access and
/// integrations. Service accounts can have API tokens generated and associated with them for
/// identification and granting extended access rights. These accounts are managed by delegated
/// administrators and can have expiry times and other auditing information attached.
/// More info:
/// https://kanidm.github.io/kanidm/master/accounts/service_accounts.html
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmServiceAccount",
    plural = "kanidmserviceaccounts",
    singular = "kanidmserviceaccount",
    shortname = "kanidmsa",
    namespaced,
    status = "KanidmServiceAccountStatus",
    doc = r#"The Kanidm service account custom resource definition (CRD) defines a service account in Kanidm."#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".status.kanidmRef"}"#,
    printcolumn = r#"{"name":"GID","type":"integer","jsonPath":".status.gid"}"#,
    printcolumn = r#"{"name":"Valid","type":"string","jsonPath":".status.conditions[?(@.type == 'Valid')].status"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmServiceAccountSpec {
    pub kanidm_ref: KanidmRef,
    pub service_account_attributes: KanidmServiceAccountAttributes,
    /// POSIX attributes for the service account. When specified, the operator will activate them.
    /// If omitted, the operator retains the attributes in the database but ceases to manage them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posix_attributes: Option<KanidmAccountPosixAttributes>,
    /// API tokens to be associated with the service account. If omitted, no tokens will be
    /// associated. If specified, the operator will ensure that the tokens exist with the
    /// specified attributes. If the tokens already exist, they will be updated to match the
    /// specified attributes. If the tokens are removed from the spec, they will be deleted from
    /// the service account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_tokens: Option<BTreeSet<KanidmAPIToken>>,

    /// Whether to generate credentials for the service account. If true, the operator will create
    /// a Kubernetes secret containing the service account's password. If false, no secret
    /// will be created. Defaults to false.
    /// Secret name: `{{ name }}-kanidm-service-account-credentials`
    #[serde(default)]
    pub generate_credentials: bool,
}

impl KanidmResource for KanidmServiceAccount {
    #[inline]
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    #[inline]
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector> {
        &kanidm.spec.service_account_namespace_selector
    }
}

/// Attributes that personally identify a service account.
///
/// The attributes defined here are set by the operator. If you want to manage those attributes
/// from the database, do not set them here.
/// Additionally, if you unset them here, they will be kept in the database.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmServiceAccountAttributes {
    /// Set the display name for the service account.
    pub displayname: String,

    /// Name/spn of a group or account that have entry manager rights over this service account.
    pub entry_managed_by: String,

    /// Set the mail address, can be set multiple times for multiple addresses. The first listed
    /// mail address is the 'primary'.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<Vec<String>>,

    /// Set an account valid from time.
    ///
    /// If omitted, the account will be valid from the time of creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_valid_from: Option<Time>,

    /// Set an accounts expiry time.
    ///
    /// If omitted, the account will not expire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_expire: Option<Time>,
}

impl PartialEq for KanidmServiceAccountAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        self.displayname == other.displayname
            && normalize_spn(&self.entry_managed_by) == normalize_spn(&other.entry_managed_by)
            // mail is not returned by kanidm currently. Link to the issue:
            // https://github.com/kanidm/kanidm/issues/3891
            // comparison is ordered because first mail is primary
            // && (self.mail.is_none() || self.mail == other.mail)
            && (self.account_valid_from.is_none()
                || self.account_valid_from == other.account_valid_from)
            && (self.account_expire.is_none() || self.account_expire == other.account_expire)
    }
}

impl From<Entry> for KanidmServiceAccountAttributes {
    fn from(entry: Entry) -> Self {
        KanidmServiceAccountAttributes {
            displayname: get_first_cloned(&entry, ATTR_DISPLAYNAME).unwrap_or_default(),
            entry_managed_by: get_first_cloned(&entry, ATTR_ENTRY_MANAGED_BY).unwrap_or_default(),
            mail: entry.attrs.get(ATTR_MAIL).cloned(),
            account_valid_from: parse_time(&entry, ATTR_ACCOUNT_VALID_FROM),
            account_expire: parse_time(&entry, ATTR_ACCOUNT_EXPIRE),
        }
    }
}

/// API token configuration for service account authentication.
///
/// API tokens can be used for identification of the service account and for granting extended
/// access rights. Tokens can be read-only or read-write, and can have expiry times and other
/// auditing information attached.
#[derive(Serialize, Deserialize, Clone, Debug, Default, Ord, PartialOrd, Eq, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmAPIToken {
    /// A string describing the token. This is not used to identify the token, it is only for human
    /// description of the tokens purpose.
    pub label: String,

    /// The purpose of the API token. Can be `readonly` (default), or `readwrite`.
    #[serde(default)]
    pub purpose: KanidmApiTokenPurpose,

    /// An optional rfc3339 time of the format "YYYY-MM-DDTHH:MM:SS+TZ",
    /// "2020-09-25T11:22:02+10:00". After this time the api token will no longer be valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<Time>,

    /// The name of the Kubernetes secret where the token value is stored.
    /// **WARNING**: A change to this field will result in a token rotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, PartialOrd, Eq, Ord)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum KanidmApiTokenPurpose {
    /// Read only tokens cannot make changes
    #[default]
    ReadOnly,
    /// Read write tokens can perform modifications on behalf of the service account.
    /// It is recommended to only use read write tokens when performing write operations to Kanidm.
    ReadWrite,
}

impl From<ApiTokenPurpose> for KanidmApiTokenPurpose {
    fn from(purpose: ApiTokenPurpose) -> Self {
        match purpose {
            ApiTokenPurpose::ReadOnly => KanidmApiTokenPurpose::ReadOnly,
            ApiTokenPurpose::ReadWrite => KanidmApiTokenPurpose::ReadWrite,
            // Synchronise tokens are just associated with system accounts
            ApiTokenPurpose::Synchronise => unreachable!(),
        }
    }
}

impl From<KanidmAPITokenStatus> for KanidmAPIToken {
    fn from(token: KanidmAPITokenStatus) -> Self {
        KanidmAPIToken {
            label: token.label,
            purpose: token.purpose,
            expiry: token.expiry,
            secret_name: token.secret_name,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, Ord, PartialOrd, Eq, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmAPITokenStatus {
    pub label: String,

    #[serde(default)]
    pub purpose: KanidmApiTokenPurpose,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<Time>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,

    /// The unique identifier for this API token in Kanidm, used for management operations.
    pub token_id: String,
}

impl KanidmAPITokenStatus {
    pub fn new(token: ApiToken, secret_name: Option<String>) -> Self {
        Self {
            label: token.label,
            purpose: KanidmApiTokenPurpose::from(token.purpose),
            expiry: token
                .expiry
                .and_then(|t| DateTime::from_timestamp(t.unix_timestamp(), 0))
                .map(Time),
            token_id: token.token_id.to_string(),
            secret_name,
        }
    }
}

/// Most recent observed status of the Kanidm Service Account. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmServiceAccountStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    pub ready: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,

    pub kanidm_ref: String,

    pub api_tokens: Vec<KanidmAPITokenStatus>,

    pub credentials_secret: Option<String>,
}
