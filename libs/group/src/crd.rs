use kanidm_proto::{constants::ATTR_GIDNUMBER, v1::Entry};
use kaniop_k8s_util::types::get_first_cloned;
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::KanidmRef;
use kaniop_operator::kanidm::crd::Kanidm;

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
