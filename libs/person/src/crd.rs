use kaniop_k8s_util::types::{get_first_cloned, parse_time};
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::{KanidmPersonPosixAttributes, KanidmRef};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_DISPLAYNAME, ATTR_LEGALNAME, ATTR_MAIL,
};
use kanidm_proto::v1::Entry;
use kube::{CustomResource, ResourceExt};
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A person represents a human's account in Kanidm. The majority of your users will be a person who
/// will use this account in their daily activities. These entries may contain personally
/// identifying information that is considered by Kanidm to be sensitive. Because of this, there
/// are default limits to who may access these data.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmPersonAccount",
    plural = "kanidmpersonsaccounts",
    singular = "kanidmpersonaccount",
    shortname = "person",
    namespaced,
    status = "KanidmPersonAccountStatus",
    doc = r#"The Kanidm person account custom resource definition (CRD) defines a person account in Kanidm.
    This resource has to be in the same namespace as the Kanidm cluster."#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".spec.kanidmRef.name"}"#,
    printcolumn = r#"{"name":"GID","type":"integer","jsonPath":".status.gid"}"#,
    printcolumn = r#"{"name":"Valid","type":"string","jsonPath":".status.conditions[?(@.type == 'Valid')].status"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAccountSpec {
    pub kanidm_ref: KanidmRef,
    pub person_attributes: KanidmPersonAttributes,
    /// POSIX attributes for the person account. When specified, the operator will activate them.
    /// If omitted, the operator retains the attributes in the database but ceases to manage them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posix_attributes: Option<KanidmPersonPosixAttributes>,
}

impl KanidmResource for KanidmPersonAccount {
    #[inline]
    fn kanidm_name(&self) -> String {
        self.spec.kanidm_ref.name.clone()
    }
    #[inline]
    fn kanidm_namespace(&self) -> String {
        // safe unwrap: person is namespaced scoped
        self.namespace().unwrap()
    }
}

/// Attributes that personally identify a person account.
///
/// The attributes defined here are set by the operator. If you want to manage those attributes
/// from the database, do not set them here.
/// Additionally, if you unset them here, they will be kept in the database.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAttributes {
    /// Set the display name for the person.
    pub displayname: String,

    /// Set the mail address, can be set multiple times for multiple addresses. The first listed
    /// mail address is the 'primary'.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<Vec<String>>,

    /// Set the legal name for the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legalname: Option<String>,

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

impl PartialEq for KanidmPersonAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        self.displayname == other.displayname
            && (self.mail.is_none() || self.mail == other.mail)
            && (self.legalname.is_none() || self.legalname == other.legalname)
            && (self.account_valid_from.is_none()
                || self.account_valid_from == other.account_valid_from)
            && (self.account_expire.is_none() || self.account_expire == other.account_expire)
    }
}

impl From<Entry> for KanidmPersonAttributes {
    fn from(entry: Entry) -> Self {
        KanidmPersonAttributes {
            displayname: get_first_cloned(&entry, ATTR_DISPLAYNAME).unwrap_or_default(),
            mail: entry.attrs.get(ATTR_MAIL).cloned(),
            legalname: get_first_cloned(&entry, ATTR_LEGALNAME),
            account_valid_from: parse_time(&entry, ATTR_ACCOUNT_VALID_FROM),
            account_expire: parse_time(&entry, ATTR_ACCOUNT_EXPIRE),
        }
    }
}

/// Most recent observed status of the Kanidm Person Account. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAccountStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
}
