use std::any::type_name;
use std::collections::HashSet;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kanidm_proto::v1::Entry;

#[inline]
pub fn normalize_spn(spn: &str) -> String {
    // safe unwrap: split always returns at least one element
    spn.split('@').next().unwrap().to_lowercase()
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
pub fn compare_names(a: &[String], b: &[String]) -> bool {
    a.iter().map(|s| normalize_spn(s)).collect::<HashSet<_>>()
        == b.iter().map(|s| normalize_spn(s)).collect::<HashSet<_>>()
}

#[inline]
pub fn normalize_url(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.as_str().to_string())
        .unwrap_or_else(|_| url.to_string())
}

#[inline]
pub fn compare_urls(a: &[String], b: &[String]) -> bool {
    a.iter().map(|s| normalize_url(s)).collect::<HashSet<_>>()
        == b.iter().map(|s| normalize_url(s)).collect::<HashSet<_>>()
}

#[inline]
fn parse_timestamp_from_string(date_str: &str) -> Result<Timestamp, k8s_openapi::jiff::Error> {
    date_str.parse()
}

pub fn get_first_cloned(entry: &Entry, key: &str) -> Option<String> {
    entry.attrs.get(key).and_then(|v| v.first().cloned())
}

pub fn get_first_as_bool(entry: &Entry, key: &str) -> Option<bool> {
    entry
        .attrs
        .get(key)
        .and_then(|v| v.first())
        .and_then(|s| s.parse().ok())
}

pub fn parse_time(entry: &Entry, key: &str) -> Option<Time> {
    entry
        .attrs
        .get(key)
        .and_then(|v| v.first())
        .and_then(|s| parse_timestamp_from_string(s).map(Time).ok())
}

#[inline]
pub fn short_type_name<K>() -> Option<&'static str> {
    let type_name = type_name::<K>();
    type_name.split("::").last()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_names, compare_urls, compare_with_spn, get_first_as_bool, get_first_cloned,
        normalize_spn, normalize_url, parse_time, parse_timestamp_from_string, short_type_name,
    };

    use std::{collections::BTreeMap, ops::Not};

    use kanidm_proto::v1::Entry;

    #[test]
    fn test_normalize() {
        assert_eq!(normalize_spn("user@domain.com"), "user".to_string());
        assert_eq!(normalize_spn("User"), "user".to_string());
        assert_eq!(normalize_spn("UsEr@domain.com"), "user".to_string());
    }

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
        assert!(compare_names(&names_or_spns, &spns));

        let spns_mismatch = vec![
            "user1@domain.com".to_string(),
            "user2@domain.com".to_string(),
            "user4@domain.com".to_string(),
        ];
        assert!(compare_names(&names_or_spns, &spns_mismatch).not());

        let spns_different_length = vec!["user1@domain.com".to_string()];
        assert!(compare_names(&names_or_spns, &spns_different_length).not());
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
    fn test_get_first_as_bool() {
        let mut attrs = BTreeMap::new();
        attrs.insert("key1".to_string(), vec!["true".to_string()]);
        attrs.insert("key2".to_string(), vec!["false".to_string()]);
        attrs.insert("key3".to_string(), vec!["invalid".to_string()]);
        let entry = Entry { attrs };

        assert_eq!(get_first_as_bool(&entry, "key1"), Some(true));
        assert_eq!(get_first_as_bool(&entry, "key2"), Some(false));
        assert_eq!(get_first_as_bool(&entry, "key3"), None);
        assert_eq!(get_first_as_bool(&entry, "key4"), None);
    }

    #[test]
    fn test_normalize_url() {
        assert_eq!(normalize_url("https://example.com"), "https://example.com/");
        assert_eq!(
            normalize_url("https://example.com/path"),
            "https://example.com/path"
        );
        assert_eq!(
            normalize_url("https://example.com/path?query=1"),
            "https://example.com/path?query=1"
        );
        assert_eq!(
            normalize_url("https://example.com/path#fragment"),
            "https://example.com/path#fragment"
        );
    }

    #[test]
    fn test_compare_urls() {
        let urls1 = vec![
            "https://example.com".to_string(),
            "app://localhost".to_string(),
            "https://example.net".to_string(),
        ];
        let urls2 = vec![
            "https://example.com/".to_string(),
            "app://localhost".to_string(),
            "https://example.net/".to_string(),
        ];
        assert!(compare_urls(&urls1, &urls2));

        let urls_mismatch = vec![
            "https://example.com/".to_string(),
            "app://localhost".to_string(),
            "https://example.com/".to_string(),
        ];
        assert!(compare_urls(&urls1, &urls_mismatch).not());

        let urls_different_length = vec!["https://example.com/".to_string()];
        assert!(compare_urls(&urls1, &urls_different_length).not());
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
    fn test_parse_timestamp_from_string() {
        let valid_date_str = "2021-09-14T12:34:56Z";
        let invalid_date_str = "invalid-date";

        assert!(parse_timestamp_from_string(valid_date_str).is_ok());
        assert!(parse_timestamp_from_string(invalid_date_str).is_err());
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
