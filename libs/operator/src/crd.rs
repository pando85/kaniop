use kaniop_k8s_util::types::get_first_cloned;

use kanidm_proto::{
    constants::{ATTR_GIDNUMBER, ATTR_LOGINSHELL},
    v1::Entry,
};

#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Checks if a given value is equal to its type's default value.
pub fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

/// KanidmRef is a reference to a Kanidm object in the same cluster. It is used to specify where
/// the object is stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRef {
    pub name: String,

    /// Only KanidmOAuth2Client can be cross-namespace. It is ignored for other resources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
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
pub struct KanidmPersonPosixAttributes {
    /// The group ID number (GID) for the person account. In Kanidm there is no difference between
    /// a UID and a GID number.
    ///
    /// If omitted, Kanidm will generate it automatically.
    ///
    /// More info:
    /// https://kanidm.github.io/kanidm/stable/accounts/posix_accounts_and_groups.html#uid-and-gid-numbers
    pub gidnumber: Option<u32>,
    /// The login shell for the person account.
    ///
    /// This sets the default shell that will be used when the user logs in via SSH or other
    /// mechanisms that require a shell. Common values include /bin/bash, /bin/zsh, /bin/sh.
    pub loginshell: Option<String>,
}

impl PartialEq for KanidmPersonPosixAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        (self.gidnumber.is_none() || self.gidnumber == other.gidnumber)
            && (self.loginshell.is_none() || self.loginshell == other.loginshell)
    }
}

impl From<Entry> for KanidmPersonPosixAttributes {
    fn from(entry: Entry) -> Self {
        KanidmPersonPosixAttributes {
            gidnumber: get_first_cloned(&entry, ATTR_GIDNUMBER).and_then(|s| s.parse::<u32>().ok()),
            loginshell: get_first_cloned(&entry, ATTR_LOGINSHELL),
        }
    }
}
