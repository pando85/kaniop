use kaniop_k8s_util::types::get_first_cloned;

use kanidm_proto::{
    constants::{ATTR_GIDNUMBER, ATTR_LOGINSHELL},
    v1::Entry,
};

#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration for automatic secret rotation.
///
/// When enabled, the operator will automatically rotate secrets based on the configured period.
/// This is useful for security compliance and reducing the impact of credential leakage.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SecretRotation {
    /// Enable automatic secret rotation. Defaults to false (opt-in).
    #[serde(default)]
    pub enabled: bool,

    /// Rotation period in days. Secrets will be rotated when they are older than this period.
    /// Defaults to 90 days.
    #[serde(default = "default_rotation_period_days")]
    pub period_days: u32,
}

impl Default for SecretRotation {
    fn default() -> Self {
        Self {
            enabled: false,
            period_days: default_rotation_period_days(),
        }
    }
}

/// Default rotation period of 90 days.
///
/// This value aligns with common security best practices and compliance frameworks
/// (e.g., PCI-DSS, SOC 2) that recommend rotating credentials every 90 days.
fn default_rotation_period_days() -> u32 {
    90
}

/// Checks if a given value is equal to its type's default value.
pub fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    #[cfg(feature = "examples-gen")]
    {
        // When generating examples, never skip fields so users can see all available options
        let _ = value; // Suppress unused variable warning
        false
    }
    #[cfg(not(feature = "examples-gen"))]
    {
        value == &T::default()
    }
}

/// KanidmRef is a reference to a Kanidm object in the same cluster. It is used to specify where
/// the object is stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[schemars(extend("x-kubernetes-validations" = [{"message": "Value is immutable", "rule": "self == oldSelf"}]))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRef {
    pub name: String,

    /// For cross-namespace resources. Reference Kanidm namespace. If omitted, the namespace of the
    /// resource will be used.
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
pub struct KanidmAccountPosixAttributes {
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

impl PartialEq for KanidmAccountPosixAttributes {
    /// Compare attributes defined in the first object with the second object values.
    /// If the second object has more attributes defined, they will be ignored.
    fn eq(&self, other: &Self) -> bool {
        (self.gidnumber.is_none() || self.gidnumber == other.gidnumber)
            && (self.loginshell.is_none() || self.loginshell == other.loginshell)
    }
}

impl From<Entry> for KanidmAccountPosixAttributes {
    fn from(entry: Entry) -> Self {
        KanidmAccountPosixAttributes {
            gidnumber: get_first_cloned(&entry, ATTR_GIDNUMBER).and_then(|s| s.parse::<u32>().ok()),
            loginshell: get_first_cloned(&entry, ATTR_LOGINSHELL),
        }
    }
}
