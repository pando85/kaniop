use std::any::type_name;
use std::ops::Not;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::chrono::{DateTime, ParseError, Utc};
use kanidm_proto::v1::Entry;

pub fn compare_with_spns(names_or_spns: &[String], spns: &[String]) -> bool {
    if names_or_spns.len() != spns.len() {
        return false;
    }
    for (a, b) in names_or_spns.iter().zip(spns.iter()) {
        if compare_with_spn(a, b).not() {
            return false;
        }
    }
    true
}

#[inline]
pub fn compare_with_spn(name_or_spn: &str, spn: &str) -> bool {
    if name_or_spn.contains("@") {
        name_or_spn.to_lowercase() == spn
    } else {
        name_or_spn.to_lowercase() == spn.split("@").next().unwrap()
    }
}

#[inline]
fn parse_datetime_from_string(date_str: &str) -> Result<DateTime<Utc>, ParseError> {
    DateTime::parse_from_rfc3339(date_str).map(|dt| dt.with_timezone(&Utc))
}

pub fn get_first_cloned(entry: &Entry, key: &str) -> Option<String> {
    entry.attrs.get(key).and_then(|v| v.first().cloned())
}

pub fn parse_time(entry: &Entry, key: &str) -> Option<Time> {
    entry
        .attrs
        .get(key)
        .and_then(|v| v.first())
        .and_then(|s| parse_datetime_from_string(s).map(Time).ok())
}

#[inline]
pub fn short_type_name<K>() -> Option<&'static str> {
    let type_name = type_name::<K>();
    type_name.split("::").last()
}

#[cfg(test)]
mod tests {
    use crate::types::{compare_with_spn, compare_with_spns, short_type_name};

    use super::{get_first_cloned, parse_datetime_from_string, parse_time};

    use std::{collections::BTreeMap, ops::Not};

    use kanidm_proto::v1::Entry;

    #[test]
    fn test_compare_with_spn() {
        assert!(compare_with_spn("user@domain.com", "user@domain.com"));
        assert!(compare_with_spn("user", "user@domain.com"));
        assert!(compare_with_spn("user@domain.com", "other@domain.com").not());
        assert!(compare_with_spn("user", "other@domain.com").not());
    }

    #[test]
    fn test_compare_with_spns() {
        let names_or_spns = vec![
            "user1@domain.com".to_string(),
            "user2".to_string(),
            "user3@domain.com".to_string(),
        ];
        let spns = vec![
            "user1@domain.com".to_string(),
            "user2@domain.com".to_string(),
            "user3@domain.com".to_string(),
        ];
        assert!(compare_with_spns(&names_or_spns, &spns));

        let spns_mismatch = vec![
            "user1@domain.com".to_string(),
            "user2@domain.com".to_string(),
            "user4@domain.com".to_string(),
        ];
        assert!(compare_with_spns(&names_or_spns, &spns_mismatch).not());

        let spns_different_length = vec!["user1@domain.com".to_string()];
        assert!(compare_with_spns(&names_or_spns, &spns_different_length).not());
    }

    #[test]
    fn test_get_first_cloned() {
        let mut attrs = BTreeMap::new();
        attrs.insert("key1".to_string(), vec!["value1".to_string()]);
        attrs.insert(
            "key2".to_string(),
            vec!["value2".to_string(), "value3".to_string()],
        );
        let entry = Entry { attrs };

        assert_eq!(get_first_cloned(&entry, "key1"), Some("value1".to_string()));
        assert_eq!(get_first_cloned(&entry, "key2"), Some("value2".to_string()));
        assert_eq!(get_first_cloned(&entry, "key3"), None);
    }

    #[test]
    fn test_parse_time() {
        let mut attrs = BTreeMap::new();
        attrs.insert("key1".to_string(), vec!["2021-09-14T12:34:56Z".to_string()]);
        attrs.insert("key2".to_string(), vec!["invalid-date".to_string()]);
        let entry = Entry { attrs };

        assert!(parse_time(&entry, "key1").is_some());
        assert!(parse_time(&entry, "key2").is_none());
        assert!(parse_time(&entry, "key3").is_none());
    }

    #[test]
    fn test_parse_datetime_from_string() {
        let valid_date_str = "2021-09-14T12:34:56Z";
        let invalid_date_str = "invalid-date";

        assert!(parse_datetime_from_string(valid_date_str).is_ok());
        assert!(parse_datetime_from_string(invalid_date_str).is_err());
    }

    #[test]
    fn test_short_type_name() {
        assert_eq!(short_type_name::<i32>(), Some("i32"));
        assert_eq!(
            short_type_name::<k8s_openapi::api::core::v1::Pod>(),
            Some("Pod")
        );
    }
}
