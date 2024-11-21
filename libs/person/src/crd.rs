use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::CustomResource;
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
    plural = "kanidmpeopleaccounts",
    singular = "kanidmpersonaccount",
    shortname = "person",
    namespaced,
    status = "KanidmPersonAccountStatus",
    doc = r#"The Kanidm person account custom resource definition (CRD) defines a person account in Kanidm.
    This resource has to be in the same namespace as the Kanidm cluster."#,
    printcolumn = r#"{"name":"Exists","type":"string","jsonPath":".status.conditions[?(@.type == 'Exists')].status"}"#,
    printcolumn = r#"{"name":"Updated","type":"string","jsonPath":".status.conditions[?(@.type == 'Updated')].status"}"#,
    printcolumn = r#"{"name":"Valid","type":"string","jsonPath":".status.conditions[?(@.type == 'Valid')].status"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAccountSpec {
    pub kanidm_ref: KanidmRef,
    pub person_attributes: KanidmPersonAttributes,
    // TODO: add posix attributes, they are optional. If not present, the person will not be posix
}

/// KanidmRef is a reference to a Kanidm object in the same namespace. It is used to specify where
/// the person account is stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRef {
    pub name: String,
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
    pub displayname: String,
    pub mail: Option<Vec<String>>,
    pub legalname: Option<String>,
    pub account_valid_from: Option<Time>,
    pub account_expire: Option<Time>,
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
}
