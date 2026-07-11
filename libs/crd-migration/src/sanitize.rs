use serde_json::{Map, Value};

use crate::{LEGACY_FINALIZER, MigrationError, Result};

pub fn sanitize_for_backup(obj: &Value) -> Result<Value> {
    validate_source_object(obj)?;
    let mut sanitized = obj.clone();

    if let Some(metadata) = sanitized
        .get_mut("metadata")
        .and_then(|m| m.as_object_mut())
    {
        metadata.remove("uid");
        metadata.remove("resourceVersion");
        metadata.remove("generation");
        metadata.remove("creationTimestamp");
        metadata.remove("managedFields");
        metadata.remove("deletionTimestamp");
        metadata.remove("deletionGracePeriodSeconds");
        metadata.remove("selfLink");

        if let Some(finalizers) = metadata
            .get_mut("finalizers")
            .and_then(|f| f.as_array_mut())
        {
            finalizers.retain(|f| f.as_str() != Some(LEGACY_FINALIZER));
            if finalizers.is_empty() {
                metadata.remove("finalizers");
            }
        }
    }

    if let Some(obj_map) = sanitized.as_object_mut() {
        obj_map.remove("status");
    }

    Ok(sanitized)
}

pub fn sanitize_for_restore(sanitized: &Value) -> Result<Value> {
    let mut restore = sanitized.clone();

    if let Some(metadata) = restore.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        metadata.remove("uid");
        metadata.remove("resourceVersion");
        metadata.remove("generation");
        metadata.remove("creationTimestamp");
        metadata.remove("managedFields");
        metadata.remove("deletionTimestamp");
        metadata.remove("deletionGracePeriodSeconds");
        metadata.remove("selfLink");
        metadata.remove("finalizers");
    }

    if let Some(obj_map) = restore.as_object_mut() {
        obj_map.remove("status");
    }

    Ok(restore)
}

fn validate_source_object(obj: &Value) -> Result<()> {
    let obj_map = obj
        .as_object()
        .ok_or_else(|| MigrationError::Sanitize("object is not a JSON object".to_string()))?;

    let api_version = obj_map
        .get("apiVersion")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MigrationError::Sanitize("missing apiVersion".to_string()))?;

    let expected_api_version = format!("{}/{}", crate::API_GROUP, crate::API_VERSION);
    if api_version != expected_api_version {
        return Err(MigrationError::Sanitize(format!(
            "unexpected apiVersion: {api_version}, expected {expected_api_version}"
        )));
    }

    let kind = obj_map
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MigrationError::Sanitize("missing kind".to_string()))?;

    if kind != crate::KIND {
        return Err(MigrationError::Sanitize(format!(
            "unexpected kind: {kind}, expected {}",
            crate::KIND
        )));
    }

    let metadata = obj_map
        .get("metadata")
        .and_then(|m| m.as_object())
        .ok_or_else(|| MigrationError::Sanitize("missing metadata".to_string()))?;

    let namespace = metadata
        .get("namespace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MigrationError::Sanitize("missing metadata.namespace".to_string()))?;

    let name = metadata
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MigrationError::Sanitize("missing metadata.name".to_string()))?;

    if namespace.is_empty() || name.is_empty() {
        return Err(MigrationError::Sanitize(
            "namespace and name must be non-empty".to_string(),
        ));
    }

    if let Some(finalizers) = metadata.get("finalizers").and_then(|f| f.as_array()) {
        for f in finalizers {
            if let Some(f_str) = f.as_str() {
                if f_str != LEGACY_FINALIZER {
                    return Err(MigrationError::UnknownFinalizer {
                        ns: namespace.to_string(),
                        name: name.to_string(),
                        finalizer: f_str.to_string(),
                    });
                }
            }
        }
    }

    Ok(())
}

pub fn compute_finalizer_patch(current: &[String]) -> Result<Option<Vec<String>>> {
    let mut seen_legacy = false;
    let mut unknown = Vec::new();

    for f in current {
        if f == LEGACY_FINALIZER {
            if seen_legacy {
                return Err(MigrationError::Sanitize(
                    "duplicate legacy finalizer".to_string(),
                ));
            }
            seen_legacy = true;
        } else {
            unknown.push(f.clone());
        }
    }

    if !unknown.is_empty() {
        return Err(MigrationError::Sanitize(format!(
            "unknown finalizers present: {:?}",
            unknown
        )));
    }

    if seen_legacy {
        Ok(Some(vec![]))
    } else {
        Ok(None)
    }
}

pub fn preserved_fields_match(source: &Value, restored: &Value) -> bool {
    let source_meta = source.get("metadata").and_then(|m| m.as_object());
    let restored_meta = restored.get("metadata").and_then(|m| m.as_object());

    match (source_meta, restored_meta) {
        (Some(s), Some(r)) => {
            fields_equal(s, r, "labels")
                && fields_equal(s, r, "annotations")
                && fields_equal(s, r, "ownerReferences")
        }
        _ => false,
    }
}

fn fields_equal(a: &Map<String, Value>, b: &Map<String, Value>, key: &str) -> bool {
    match (a.get(key), b.get(key)) {
        (Some(av), Some(bv)) => av == bv,
        (None, None) => true,
        (Some(av), None) | (None, Some(av)) => {
            av.as_object().is_some_and(|o| o.is_empty())
                || av.as_array().is_some_and(|a| a.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_person() -> Value {
        json!({
            "apiVersion": "kaniop.rs/v1beta1",
            "kind": "KanidmPersonAccount",
            "metadata": {
                "name": "alice",
                "namespace": "default",
                "uid": "abc-123",
                "resourceVersion": "42",
                "generation": 1,
                "creationTimestamp": "2024-01-01T00:00:00Z",
                "managedFields": [{"manager": "kubectl"}],
                "labels": {
                    "app.kubernetes.io/managed-by": "argocd"
                },
                "annotations": {
                    "argocd.argoproj.io/tracking-id": "kaniop:KanidmPersonAccount:default/alice"
                },
                "finalizers": ["kanidmpersonsaccounts.kaniop.rs/finalizer"],
                "ownerReferences": [{
                    "apiVersion": "kaniop.rs/v1beta1",
                    "kind": "Kanidm",
                    "name": "kanidm",
                    "uid": "owner-uid"
                }]
            },
            "spec": {
                "kanidmRef": {
                    "name": "kanidm"
                },
                "displayName": "Alice"
            },
            "status": {
                "ready": true
            }
        })
    }

    #[test]
    fn test_sanitize_removes_server_managed_fields() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();

        let meta = sanitized.get("metadata").unwrap().as_object().unwrap();
        assert!(meta.get("uid").is_none());
        assert!(meta.get("resourceVersion").is_none());
        assert!(meta.get("generation").is_none());
        assert!(meta.get("creationTimestamp").is_none());
        assert!(meta.get("managedFields").is_none());
    }

    #[test]
    fn test_sanitize_preserves_labels_annotations_owners() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();

        let meta = sanitized.get("metadata").unwrap().as_object().unwrap();
        assert!(meta.get("labels").is_some());
        assert!(meta.get("annotations").is_some());
        assert!(meta.get("ownerReferences").is_some());
    }

    #[test]
    fn test_sanitize_removes_status() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();
        assert!(sanitized.get("status").is_none());
    }

    #[test]
    fn test_sanitize_removes_known_finalizer() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();

        let meta = sanitized.get("metadata").unwrap().as_object().unwrap();
        assert!(meta.get("finalizers").is_none());
    }

    #[test]
    fn test_sanitize_preserves_spec() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();
        assert_eq!(sanitized.get("spec"), obj.get("spec"));
    }

    #[test]
    fn test_sanitize_rejects_unknown_finalizer() {
        let mut obj = sample_person();
        obj["metadata"]["finalizers"] = json!([
            "kanidmpersonsaccounts.kaniop.rs/finalizer",
            "some.other/finalizer"
        ]);

        let result = sanitize_for_backup(&obj);
        assert!(result.is_err());
        match result.unwrap_err() {
            MigrationError::UnknownFinalizer { finalizer, .. } => {
                assert_eq!(finalizer, "some.other/finalizer");
            }
            e => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn test_sanitize_rejects_wrong_api_version() {
        let mut obj = sample_person();
        obj["apiVersion"] = json!("wrong/v1");

        let result = sanitize_for_backup(&obj);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_rejects_wrong_kind() {
        let mut obj = sample_person();
        obj["kind"] = json!("WrongKind");

        let result = sanitize_for_backup(&obj);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_rejects_missing_name() {
        let mut obj = sample_person();
        obj["metadata"]["name"] = json!(null);

        let result = sanitize_for_backup(&obj);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_for_restore_removes_finalizers() {
        let obj = sample_person();
        let sanitized = sanitize_for_backup(&obj).unwrap();
        let restore = sanitize_for_restore(&sanitized).unwrap();

        let meta = restore.get("metadata").unwrap().as_object().unwrap();
        assert!(meta.get("finalizers").is_none());
    }

    #[test]
    fn test_compute_finalizer_patch_only_known() {
        let current = vec![LEGACY_FINALIZER.to_string()];
        let patch = compute_finalizer_patch(&current).unwrap();
        assert_eq!(patch, Some(vec![]));
    }

    #[test]
    fn test_compute_finalizer_patch_empty() {
        let current: Vec<String> = vec![];
        let patch = compute_finalizer_patch(&current).unwrap();
        assert_eq!(patch, None);
    }

    #[test]
    fn test_compute_finalizer_patch_unknown_present() {
        let current = vec!["other/finalizer".to_string(), LEGACY_FINALIZER.to_string()];
        let result = compute_finalizer_patch(&current);
        assert!(result.is_err());
    }

    #[test]
    fn test_compute_finalizer_patch_duplicate_legacy() {
        let current = vec![LEGACY_FINALIZER.to_string(), LEGACY_FINALIZER.to_string()];
        let result = compute_finalizer_patch(&current);
        assert!(result.is_err());
    }

    #[test]
    fn test_preserved_fields_match() {
        let source = sample_person();
        let mut restored = source.clone();
        restored["metadata"]["uid"] = json!("new-uid");

        assert!(preserved_fields_match(&source, &restored));
    }

    #[test]
    fn test_preserved_fields_mismatch_labels() {
        let source = sample_person();
        let mut restored = source.clone();
        restored["metadata"]["labels"] = json!({"different": "label"});

        assert!(!preserved_fields_match(&source, &restored));
    }
}
