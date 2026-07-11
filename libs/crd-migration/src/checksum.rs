use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical_json(value: &Value) -> Vec<u8> {
    let canonical = canonicalize(value);
    canonical.into_bytes()
}

fn canonicalize(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", escape_json_string(s)),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(canonicalize).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let items: Vec<String> = keys
                .iter()
                .map(|k| format!("\"{}\":{}", escape_json_string(k), canonicalize(&map[*k])))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn object_checksum(value: &Value) -> String {
    sha256_hex(&canonical_json(value))
}

pub fn source_set_checksum(entries: &[(String, String, String)]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    let mut hasher = Sha256::new();
    for (ns, name, obj_checksum) in &sorted {
        hasher.update(format!("{ns}/{name}/{obj_checksum}\n").as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub fn empty_source_set_checksum() -> String {
    source_set_checksum(&[])
}

pub fn backup_secret_name(prefix: &str, namespace: &str, name: &str) -> String {
    let input = format!("{namespace}/{name}");
    let hash = sha256_hex(input.as_bytes());
    format!("{prefix}{}", &hash[..20])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_canonical_json_field_order_invariant() {
        let a = json!({"b": 2, "a": 1});
        let b = json!({"a": 1, "b": 2});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn test_canonical_json_list_order_matters() {
        let a = json!([1, 2, 3]);
        let b = json!([3, 2, 1]);
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn test_object_checksum_identical() {
        let a = json!({"apiVersion": "v1", "kind": "Test", "metadata": {"name": "foo"}});
        let b = json!({"metadata": {"name": "foo"}, "kind": "Test", "apiVersion": "v1"});
        assert_eq!(object_checksum(&a), object_checksum(&b));
    }

    #[test]
    fn test_object_checksum_different_spec() {
        let a = json!({"spec": {"field": "value1"}});
        let b = json!({"spec": {"field": "value2"}});
        assert_ne!(object_checksum(&a), object_checksum(&b));
    }

    #[test]
    fn test_source_set_checksum_deterministic() {
        let entries_a = vec![
            (
                "ns1".to_string(),
                "name1".to_string(),
                "checksum1".to_string(),
            ),
            (
                "ns2".to_string(),
                "name2".to_string(),
                "checksum2".to_string(),
            ),
        ];
        let entries_b = vec![
            (
                "ns2".to_string(),
                "name2".to_string(),
                "checksum2".to_string(),
            ),
            (
                "ns1".to_string(),
                "name1".to_string(),
                "checksum1".to_string(),
            ),
        ];
        assert_eq!(
            source_set_checksum(&entries_a),
            source_set_checksum(&entries_b)
        );
    }

    #[test]
    fn test_source_set_checksum_different_content() {
        let entries_a = vec![(
            "ns1".to_string(),
            "name1".to_string(),
            "checksum1".to_string(),
        )];
        let entries_b = vec![(
            "ns1".to_string(),
            "name1".to_string(),
            "checksum2".to_string(),
        )];
        assert_ne!(
            source_set_checksum(&entries_a),
            source_set_checksum(&entries_b)
        );
    }

    #[test]
    fn test_backup_secret_name_deterministic() {
        let name_a = backup_secret_name("kaniop-person-backup-", "default", "alice");
        let name_b = backup_secret_name("kaniop-person-backup-", "default", "alice");
        assert_eq!(name_a, name_b);
        assert!(name_a.starts_with("kaniop-person-backup-"));
        assert_eq!(name_a.len(), "kaniop-person-backup-".len() + 20);
    }

    #[test]
    fn test_backup_secret_name_different_sources() {
        let name_a = backup_secret_name("kaniop-person-backup-", "default", "alice");
        let name_b = backup_secret_name("kaniop-person-backup-", "default", "bob");
        assert_ne!(name_a, name_b);
    }

    #[test]
    fn test_sha256_hex_length() {
        let hash = sha256_hex(b"test");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_canonical_json_nested() {
        let a = json!({"z": {"b": 2, "a": 1}, "a": [3, 2, 1]});
        let b = json!({"a": [3, 2, 1], "z": {"a": 1, "b": 2}});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn test_empty_source_set_checksum() {
        let empty = empty_source_set_checksum();
        assert!(!empty.is_empty());
        assert_eq!(empty.len(), 64);
        let empty2 = empty_source_set_checksum();
        assert_eq!(empty, empty2);
    }

    #[test]
    fn test_escape_json_string() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_json_string("he\\llo"), "he\\\\llo");
        assert_eq!(escape_json_string("he\nllo"), "he\\nllo");
    }
}
