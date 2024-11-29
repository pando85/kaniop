#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// KanidmRef is a reference to a Kanidm object in the same namespace. It is used to specify where
/// the person account is stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRef {
    pub name: String,
}
