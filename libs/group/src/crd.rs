use kanidm_proto::{constants::ATTR_GIDNUMBER, v1::Entry};
use kaniop_k8s_util::types::get_first_cloned;
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
    kind = "KanidmGroup",
    plural = "groups",
    singular = "kanidmgroup",
    shortname = "kg",
    namespaced,
    status = "KanidmGroupStatus",
    doc = r#"The Kanidm group custom resource definition (CRD) defines a group in Kanidm.
    This resource has to be in the same namespace as the Kanidm cluster."#,
    printcolumn = r#"{"name":"Exists","type":"string","jsonPath":".status.conditions[?(@.type == 'Exists')].status"}"#,
    printcolumn = r#"{"name":"ManagedBy","type":"string","jsonPath":".spec.entryManagedBy"}"#,
    printcolumn = r#"{"name":"Updated","type":"string","jsonPath":".spec.conditions[?(@.type == 'Updated')].status"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmGroupSpec {
    pub kanidm_ref: KanidmRef,

    /// Optional name/spn of a group that have entry manager rights over this group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_managed_by: Option<String>,

    /// Set the exact list of mail addresses that this group is associated with. The first mail
    /// address in the list is the `primary` and the remainder are aliases. Setting an empty list
    /// will clear the mail attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<Vec<String>>,

    /// Set the exact list of members that this group should contain, removing any not listed in
    /// the set operation.
    /// If you want to manage members from the database, do not set them here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,

    /// POSIX attributes for the group account. When specified, the operator will activate them.
    /// If omitted, the operator retains the attributes in the database but ceases to manage them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posix_attributes: Option<KanidmGroupPosixAttributes>,
}

impl KanidmResource for KanidmGroup {
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
pub struct KanidmGroupStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,
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
