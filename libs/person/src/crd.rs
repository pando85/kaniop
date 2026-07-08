use kaniop_k8s_util::types::{compare_mail, get_first_cloned, parse_time};
use kaniop_operator::controller::kanidm::KanidmResource;
use kaniop_operator::crd::{is_default, KanidmAccountPosixAttributes, KanidmRef};
use kaniop_operator::kanidm::crd::Kanidm;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector, Time};
use kanidm_proto::constants::{
    ATTR_ACCOUNT_EXPIRE, ATTR_ACCOUNT_VALID_FROM, ATTR_DISPLAYNAME, ATTR_LEGALNAME, ATTR_MAIL,
};
use kanidm_proto::v1::Entry;
use kube::CustomResource;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A person represents a human's account in Kanidm. The majority of your users will be a person who
/// will use this account in their daily activities. These entries may contain personally
/// identifying information that is considered by Kanidm to be sensitive. Because of this, there
/// are default limits to who may access these data.
/// More info:
/// https://kanidm.github.io/kanidm/master/accounts/people_accounts.html
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmPersonAccount",
    plural = "kanidmpersonaccounts",
    singular = "kanidmpersonaccount",
    shortname = "person",
    namespaced,
    status = "KanidmPersonAccountStatus",
    doc = r#"The Kanidm person account custom resource definition (CRD) defines a person account in Kanidm."#,
    printcolumn = r#"{"name":"Kanidm","type":"string","jsonPath":".status.kanidmRef"}"#,
    printcolumn = r#"{"name":"GID","type":"integer","jsonPath":".status.gid"}"#,
    printcolumn = r#"{"name":"Valid","type":"string","jsonPath":".status.conditions[?(@.type == 'Valid')].status"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAccountSpec {
    pub kanidm_ref: KanidmRef,

    /// The name of the entity in Kanidm. If not specified, the Kubernetes resource name is used.
    /// Use this field to manage Kanidm entities with names that don't conform to Kubernetes naming rules
    /// (e.g., entities with underscores like `idm_admin` or `idm_all_persons`).
    /// This field is immutable and cannot be changed after creation.
    #[schemars(extend("x-kubernetes-validations" = [{"message": "kanidmName cannot be changed.", "rule": "self == oldSelf"}]))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kanidm_name: Option<String>,

    pub person_attributes: KanidmPersonAttributes,
    /// POSIX attributes for the person account. When specified, the operator will activate them.
    /// If omitted, the operator retains the attributes in the database but ceases to manage them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posix_attributes: Option<KanidmAccountPosixAttributes>,

    /// If credentials are not defined, Kaniop will generate a link accessible when describing
    /// the person account. This link will be valid for the number of seconds defined here.
    /// The default is 3600 seconds (1 hour).
    #[serde(
        default = "default_credentials_token_ttl",
        skip_serializing_if = "is_default"
    )]
    pub credentials_token_ttl: u32,
}

impl KanidmResource for KanidmPersonAccount {
    #[inline]
    fn kanidm_ref_spec(&self) -> &KanidmRef {
        &self.spec.kanidm_ref
    }

    #[inline]
    fn get_namespace_selector(kanidm: &Kanidm) -> &Option<LabelSelector> {
        &kanidm.spec.person_namespace_selector
    }

    #[inline]
    fn kanidm_name_override(&self) -> Option<&str> {
        self.spec.kanidm_name.as_deref()
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
    fn eq(&self, other: &Self) -> bool {
        self.displayname == other.displayname
            && (self.mail.is_none()
                || self.mail.as_ref().is_some_and(|m| {
                    other.mail.as_ref().is_some_and(|o| compare_mail(m, o))
                }))
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

fn default_credentials_token_ttl() -> u32 {
    3600
}

/// Most recent observed status of the Kanidm Person Account. Read-only.
/// More info:
/// https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#spec-and-status
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmPersonAccountStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    pub ready: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,

    pub kanidm_ref: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanidm_proto::v1::Entry;
    use std::collections::BTreeMap;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use k8s_openapi::jiff::Timestamp;

    fn create_entry(attrs: BTreeMap<String, Vec<String>>) -> Entry {
        Entry { attrs }
    }

    #[test]
    fn exact_match() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap(); // 2023-01-01T00:00:00Z
        let timestamp2 = Timestamp::from_second(1704067200).unwrap(); // 2024-01-01T00:00:00Z
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn displayname_missing_in_kanidm() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        // Missing displayname attribute
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // The kanidm_attrs.displayname will be "" due to unwrap_or_default()
        // So "Alice" != "" -> not equal
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn displayname_different() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Bob".to_string()]); // Different from spec
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_missing_in_kanidm() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]), // Spec has mail
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        // Missing mail attribute
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // The kanidm_attrs.mail will be None
        // So Some(...) != None -> not equal
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_different_count() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string(), "a@test.com".to_string()]), // 2 emails
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]); // Only 1 email
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_reordered() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["primary@example.com".to_string(), "secondary@example.com".to_string(), "third@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["primary@example.com".to_string(), "third@example.com".to_string(), "secondary@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_primary_different() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["primary@example.com".to_string(), "secondary@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["secondary@example.com".to_string(), "primary@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_spec_none_kanidm_has_mail() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: None, // Spec doesn't care about mail
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["x@example.com".to_string()]); // Kanidm has mail
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // Since spec.mail.is_none(), the comparison should skip mail
        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn legalname_spec_some_kanidm_missing() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()), // Spec has legalname
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        // Missing legalname attribute
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // The kanidm_attrs.legalname will be None
        // So Some("Alice Cooper") != None -> not equal
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn legalname_spec_none_kanidm_has_value() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: None, // Spec doesn't care about legalname
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]); // Kanidm has legalname
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // Since spec.legalname.is_none(), the comparison should skip legalname
        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn account_valid_from_matching() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn account_valid_from_spec_set_kanidm_missing() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)), // Spec has time
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        // Missing account_valid_from attribute
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));
        // The kanidm_attrs.account_valid_from will be None
        // So Some(time) != None -> not equal
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn account_expire_matching() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap();
        let timestamp2 = Timestamp::from_second(1704067200).unwrap();
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn minimal_spec() {
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: None,
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        // Other attributes are missing

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn timestamp_format_roundtrip() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap(); // 2023-01-01T00:00:00Z
        let timestamp2 = Timestamp::from_second(1704067200).unwrap(); // 2024-01-01T00:00:00Z
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00Z".to_string()]);
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn timestamp_different_format() {
        let timestamp1 = Timestamp::from_second(1672531200).unwrap(); // 2023-01-01T00:00:00Z
        let timestamp2 = Timestamp::from_second(1704067200).unwrap(); // 2024-01-01T00:00:00Z
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Cooper".to_string()),
            account_valid_from: Some(Time(timestamp1)),
            account_expire: Some(Time(timestamp2)),
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec!["alice@example.com".to_string()]);
        attrs_map.insert("legalname".to_string(), vec!["Alice Cooper".to_string()]);
        attrs_map.insert("account_valid_from".to_string(), vec!["2023-01-01T00:00:00+00:00".to_string()]); // Different format
        attrs_map.insert("account_expire".to_string(), vec!["2024-01-01T00:00:00Z".to_string()]);

        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn displayname_spec_empty_vs_kanidm_missing() {
        // This is a critical test for the bug scenario
        let spec_attrs = KanidmPersonAttributes {
            displayname: "".to_string(), // Spec has empty string
            mail: None,
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let attrs_map = BTreeMap::new();
        // No displayname attribute at all in Kanidm
        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        // Kanidm will have displayname as "" (due to unwrap_or_default())
        // So "" == "" should be true
        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn displayname_spec_vs_kanidm_empty() {
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: None,
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["".to_string()]); // Kanidm has empty string
        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        // "Alice" != "" should be false
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_spec_none_vs_kanidm_empty_vec() {
        // Test when spec has None but Kanidm has empty vec (not None)
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: None, // Spec doesn't care about mail
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec![]); // Kanidm has empty mail vector
        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        // Since spec.mail.is_none(), the comparison should skip mail
        assert_eq!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn mail_spec_some_vs_kanidm_empty_vec() {
        // Test when spec has Some but Kanidm has empty vec
        let spec_attrs = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["test@example.com".to_string()]), // Spec has mail
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let mut attrs_map = BTreeMap::new();
        attrs_map.insert("displayname".to_string(), vec!["Alice".to_string()]);
        attrs_map.insert("mail".to_string(), vec![]); // Kanidm has empty mail vector
        let kanidm_attrs = KanidmPersonAttributes::from(create_entry(attrs_map));

        // Some([..]) != Some([]) should be false
        assert_ne!(spec_attrs, kanidm_attrs);
    }

    #[test]
    fn partial_eq_asymmetry_is_intentional() {
        // PartialEq is intentionally asymmetric for the "spec vs state" comparison pattern:
        // - When comparing spec (self) against state (other), if spec has None for a field,
        //   we skip comparison (operator doesn't manage that field)
        // - This allows: spec.mail=None + state.mail=Some(...) to be "equal" (no reconcile needed)
        // - The operator always compares spec vs state in that order, never reversed

        let spec = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: None,
            account_valid_from: None,
            account_expire: None,
        };

        let state = KanidmPersonAttributes {
            displayname: "Alice".to_string(),
            mail: Some(vec!["alice@example.com".to_string()]),
            legalname: Some("Alice Smith".to_string()),
            account_valid_from: None,
            account_expire: None,
        };

        // spec == state: legalname is None in spec, so comparison is skipped -> equal
        assert_eq!(spec, state);

        // state == spec: legalname is Some in state, compared against None in spec -> not equal
        assert_ne!(state, spec);
    }
}
