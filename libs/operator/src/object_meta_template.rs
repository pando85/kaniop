use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::hash::Hash;

use k8s_openapi::NamespaceResourceScope;
use kaniop_k8s_util::error::Result;
use kube::{
    Client, Resource, ResourceExt,
    api::{ObjectMeta, PartialObjectMeta},
    runtime::reflector::Lookup,
};
use serde::{Deserialize, Serialize};

use crate::{
    controller::context::KubeOperations, crd::MetadataTemplate, metrics::ControllerMetrics,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagedMetadataKeys {
    pub annotations: BTreeSet<String>,
    pub labels: BTreeSet<String>,
}

/// Extracts the metadata keys owned by `manager` from the managed-fields list of `meta`.
/// Only entries with `operation = "Apply"` and no subresource are considered.
fn managed_metadata_keys<K>(meta: &PartialObjectMeta<K>, manager: &str) -> ManagedMetadataKeys {
    let mut keys = ManagedMetadataKeys::default();

    let Some(managed_fields) = meta.metadata.managed_fields.as_ref() else {
        return keys;
    };

    for entry in managed_fields.iter().filter(|f| {
        f.operation.as_deref() == Some("Apply")
            && f.subresource.is_none()
            && f.manager.as_deref() == Some(manager)
    }) {
        let Some(field_meta) = entry
            .fields_v1
            .as_ref()
            .and_then(|f| f.0.as_object())
            .and_then(|o| o.get("f:metadata"))
            .and_then(|m| m.as_object())
        else {
            continue;
        };

        if let Some(labels) = field_meta.get("f:labels").and_then(|l| l.as_object()) {
            for k in labels.keys() {
                if let Some(stripped) = k.strip_prefix("f:").filter(|&c| c != ".") {
                    keys.labels.insert(stripped.to_string());
                }
            }
        }
        if let Some(annotations) = field_meta.get("f:annotations").and_then(|a| a.as_object()) {
            for k in annotations.keys() {
                if let Some(stripped) = k.strip_prefix("f:").filter(|&c| c != ".") {
                    keys.annotations.insert(stripped.to_string());
                }
            }
        }
    }

    keys
}

/// Result of filtering a [`MetadataTemplate`] against the set of fields already owned by the
/// main operator manager. Labels and annotations that would conflict are separated out so callers
/// can apply the safe subset and surface the discarded keys in logs or events.
#[derive(Default)]
pub struct FilteredMetadata {
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    /// Label keys from the template that were discarded because the main operator already owns them.
    pub discarded_labels: BTreeSet<String>,
    /// Annotation keys from the template that were discarded because the main operator already owns them.
    pub discarded_annotations: BTreeSet<String>,
}

impl FilteredMetadata {
    pub fn has_discards(&self) -> bool {
        !self.discarded_labels.is_empty() || !self.discarded_annotations.is_empty()
    }
}

impl MetadataTemplate {
    /// Returns the labels and annotations that are safe to apply — i.e. those not already owned
    /// by the main operator manager — alongside any keys that were discarded due to conflicts.
    fn filter_owned(&self, owned: &ManagedMetadataKeys) -> FilteredMetadata {
        let (labels, discarded_labels) = split_owned(self.labels.as_ref(), &owned.labels);
        let (annotations, discarded_annotations) =
            split_owned(self.annotations.as_ref(), &owned.annotations);
        FilteredMetadata {
            labels,
            annotations,
            discarded_labels,
            discarded_annotations,
        }
    }
}

/// Partitions `source` into keys the template manager may own (not in `owned`) and keys it
/// may not (already claimed by the main manager). Returns `(kept, discarded)`.
fn split_owned(
    source: Option<&BTreeMap<String, String>>,
    owned: &BTreeSet<String>,
) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let Some(m) = source else {
        return Default::default();
    };
    let mut kept = BTreeMap::new();
    let mut discarded = BTreeSet::new();
    for (k, v) in m {
        if owned.contains(k) {
            discarded.insert(k.clone());
        } else {
            kept.insert(k.clone(), v.clone());
        }
    }
    (kept, discarded)
}

// ---------------------------------------------------------------------------
// ObjectMetaTemplateExt – template application trait
// ---------------------------------------------------------------------------

/// Implemented by operator resources that manage a child Kubernetes object whose metadata
/// (labels and annotations) can be customized via a [`MetadataTemplate`].
///
/// Generic over `K` so the same mechanism can serve any resource type that has `ObjectMeta`
/// (Secret, ConfigMap, …). The default implementations of [`needs_meta_template_apply`] and
/// [`apply_meta_template`] cover the common SSA cleanup/apply flow; implementors only need to
/// supply the three required methods.
pub trait ObjectMetaTemplateExt<K>
where
    Self: KubeOperations<Self, K> + ResourceExt + Clone + 'static,
    <Self as Lookup>::DynamicType: Eq + Hash + Clone,
    K: Resource<Scope = NamespaceResourceScope>
        + Default
        + Clone
        + Debug
        + Serialize
        + for<'de> Deserialize<'de>,
    <K as Resource>::DynamicType: Default,
{
    /// The SSA field-manager name used for the main operator apply.
    const OPERATOR_NAME: &'static str;
    /// Name of the managed child object.
    fn managed_object_name(&self) -> String;
    /// Template supplying extra labels/annotations, if configured.
    fn metadata_template(&self) -> Option<&MetadataTemplate>;

    /// Name of the SSA field manager used for template-owned metadata fields.
    fn template_operator_name() -> String {
        format!("template.{}", Self::OPERATOR_NAME)
    }

    /// Checks whether the live object's metadata matches the desired template state and, if not,
    /// returns the pre-filtered [`FilteredMetadata`] ready to pass to [`apply_meta_template`].
    ///
    /// Reads the managed-fields list for both the main operator and the template manager,
    /// covering all three out-of-sync cases:
    /// - A desired key is missing or has the wrong value.
    /// - The desired template is empty but the template manager still owns fields (template removed).
    /// - The desired template shrank and the template manager owns stale keys.
    fn needs_meta_template_apply(&self, live: &PartialObjectMeta<K>) -> Option<FilteredMetadata> {
        let main_keys = managed_metadata_keys(live, Self::OPERATOR_NAME);
        let template_keys = managed_metadata_keys(live, &Self::template_operator_name());
        let filtered = self
            .metadata_template()
            .map(|t| t.filter_owned(&main_keys))
            .unwrap_or_default();

        let value_mismatch =
            |desired: &BTreeMap<String, String>, actual: Option<&BTreeMap<String, String>>| {
                desired
                    .iter()
                    .any(|(k, v)| actual.and_then(|m| m.get(k)) != Some(v))
            };
        if value_mismatch(&filtered.labels, live.metadata.labels.as_ref())
            || value_mismatch(&filtered.annotations, live.metadata.annotations.as_ref())
        {
            return Some(filtered);
        }

        if filtered.labels.is_empty() && filtered.annotations.is_empty() {
            // No desired fields: in sync only if the template manager owns nothing.
            if template_keys.labels.is_empty() && template_keys.annotations.is_empty() {
                return None;
            }
            return Some(filtered);
        }

        // Desired fields non-empty: also verify the template manager owns *exactly* the desired
        // keys — no stale extras left over from a previously larger template.
        let desired_labels: BTreeSet<String> = filtered.labels.keys().cloned().collect();
        let desired_annotations: BTreeSet<String> = filtered.annotations.keys().cloned().collect();
        if template_keys.labels == desired_labels
            && template_keys.annotations == desired_annotations
        {
            None
        } else {
            Some(filtered)
        }
    }

    /// Applies `filtered` to the managed child object using the template SSA field manager.
    ///
    /// Passing an empty [`FilteredMetadata`] (no labels, no annotations) causes SSA to release any
    /// previously template-owned keys. Obtain `filtered` from [`needs_meta_template_apply`].
    /// Callers are responsible for surfacing any discarded keys (see [`FilteredMetadata::has_discards`]).
    #[allow(async_fn_in_trait)]
    async fn apply_meta_template(
        &self,
        client: Client,
        metrics: &ControllerMetrics,
        filtered: FilteredMetadata,
    ) -> Result<K> {
        // safe unwrap: implementor is namespaced
        let namespace = self.namespace().unwrap();

        let obj = build_object_metadata(
            self.managed_object_name(),
            namespace,
            filtered.labels,
            filtered.annotations,
        );
        self.kube_patch(client, metrics, obj, &Self::template_operator_name())
            .await
    }
}

/// Builds an object containing only pre-filtered template metadata for SSA via the template
/// manager. Passing empty maps causes SSA to release any previously template-owned keys.
fn build_object_metadata<K: Resource + Default>(
    name: String,
    namespace: String,
    labels: BTreeMap<String, String>,
    annotations: BTreeMap<String, String>,
) -> K {
    let mut obj = K::default();
    *obj.meta_mut() = ObjectMeta {
        name: Some(name),
        namespace: Some(namespace),
        labels: if labels.is_empty() {
            None
        } else {
            Some(labels)
        },
        annotations: if annotations.is_empty() {
            None
        } else {
            Some(annotations)
        },
        ..ObjectMeta::default()
    };
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
        FieldsV1, ManagedFieldsEntry, ObjectMeta,
    };
    use serde_json::json;

    fn meta_with_managed_fields(
        manager: &str,
        operation: &str,
        subresource: Option<&str>,
        fields: serde_json::Value,
    ) -> PartialObjectMeta<Secret> {
        PartialObjectMeta {
            metadata: ObjectMeta {
                managed_fields: Some(vec![ManagedFieldsEntry {
                    manager: Some(manager.to_string()),
                    operation: Some(operation.to_string()),
                    subresource: subresource.map(str::to_string),
                    fields_v1: Some(FieldsV1(fields)),
                    fields_type: Some("FieldsV1".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_extracts_label_keys() {
        let fields = json!({
            "f:metadata": {
                "f:labels": {
                    ".": {},
                    "f:app.kubernetes.io/name": {},
                    "f:example.com/foo": {},
                }
            }
        });
        let meta = meta_with_managed_fields("test-manager", "Apply", None, fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(keys.labels.contains("app.kubernetes.io/name"));
        assert!(keys.labels.contains("example.com/foo"));
        assert!(
            !keys.labels.contains("."),
            "existence marker must be filtered out"
        );
        assert!(keys.annotations.is_empty());
    }

    #[test]
    fn test_extracts_annotation_keys() {
        let fields = json!({
            "f:metadata": {
                "f:annotations": {
                    ".": {},
                    "f:kaniop.rs/rotation-enabled": {},
                    "f:example.com/note": {},
                }
            }
        });
        let meta = meta_with_managed_fields("test-manager", "Apply", None, fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(keys.annotations.contains("kaniop.rs/rotation-enabled"));
        assert!(keys.annotations.contains("example.com/note"));
        assert!(keys.labels.is_empty());
    }

    #[test]
    fn test_ignores_wrong_manager() {
        let fields = json!({ "f:metadata": { "f:labels": { "f:example.com/foo": {} } } });
        let meta = meta_with_managed_fields("other-manager", "Apply", None, fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(
            keys.labels.is_empty(),
            "keys from a different manager must not be returned"
        );
    }

    #[test]
    fn test_ignores_update_operation() {
        let fields = json!({ "f:metadata": { "f:labels": { "f:example.com/foo": {} } } });
        // "Update" operation entries must not be considered for SSA staleness checks.
        let meta = meta_with_managed_fields("test-manager", "Update", None, fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(
            keys.labels.is_empty(),
            "Update-operation entries must not be returned"
        );
    }

    #[test]
    fn test_ignores_subresource_entry() {
        let fields = json!({ "f:metadata": { "f:labels": { "f:example.com/foo": {} } } });
        // e.g. a status subresource entry must not be included.
        let meta = meta_with_managed_fields("test-manager", "Apply", Some("status"), fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(
            keys.labels.is_empty(),
            "subresource entries must not be returned"
        );
    }

    #[test]
    fn test_no_metadata_fields_returns_none() {
        // The manager owns only data fields, not metadata labels or annotations.
        let fields = json!({ "f:data": { ".": {}, "f:CLIENT_ID": {}, "f:CLIENT_SECRET": {} } });
        let meta = meta_with_managed_fields("test-manager", "Apply", None, fields);
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(keys.labels.is_empty());
        assert!(keys.annotations.is_empty());
    }

    #[test]
    fn test_no_managed_fields_returns_none() {
        let meta: PartialObjectMeta<Secret> = PartialObjectMeta::default();
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(keys.labels.is_empty());
        assert!(keys.annotations.is_empty());
    }

    #[test]
    fn test_multiple_entries_for_same_manager_are_accumulated() {
        // The same manager can appear more than once in managed_fields when separate Apply
        // operations owned different subsets of fields (e.g. labels in one apply, annotations
        // in a later apply). All entries must be accumulated into a single result.
        let meta: PartialObjectMeta<Secret> = PartialObjectMeta {
            metadata: ObjectMeta {
                managed_fields: Some(vec![
                    ManagedFieldsEntry {
                        manager: Some("test-manager".to_string()),
                        operation: Some("Apply".to_string()),
                        subresource: None,
                        fields_v1: Some(FieldsV1(json!({
                            "f:metadata": {
                                "f:labels": {
                                    ".": {},
                                    "f:app.kubernetes.io/name": {},
                                }
                            }
                        }))),
                        fields_type: Some("FieldsV1".to_string()),
                        ..Default::default()
                    },
                    ManagedFieldsEntry {
                        manager: Some("test-manager".to_string()),
                        operation: Some("Apply".to_string()),
                        subresource: None,
                        fields_v1: Some(FieldsV1(json!({
                            "f:metadata": {
                                "f:annotations": {
                                    ".": {},
                                    "f:example.com/note": {},
                                }
                            }
                        }))),
                        fields_type: Some("FieldsV1".to_string()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let keys = managed_metadata_keys(&meta, "test-manager");
        assert!(
            keys.labels.contains("app.kubernetes.io/name"),
            "label from first entry must be present"
        );
        assert!(
            keys.annotations.contains("example.com/note"),
            "annotation from second entry must be present"
        );
    }

    #[test]
    fn test_extracts_keys_for_each_manager_independently() {
        let meta: PartialObjectMeta<Secret> = PartialObjectMeta {
            metadata: ObjectMeta {
                managed_fields: Some(vec![
                    ManagedFieldsEntry {
                        manager: Some("operator".to_string()),
                        operation: Some("Apply".to_string()),
                        subresource: None,
                        fields_v1: Some(FieldsV1(json!({
                            "f:metadata": {
                                "f:labels": { ".": {}, "f:app.kubernetes.io/name": {} }
                            }
                        }))),
                        fields_type: Some("FieldsV1".to_string()),
                        ..Default::default()
                    },
                    ManagedFieldsEntry {
                        manager: Some("template.operator".to_string()),
                        operation: Some("Apply".to_string()),
                        subresource: None,
                        fields_v1: Some(FieldsV1(json!({
                            "f:metadata": {
                                "f:labels": { ".": {}, "f:example.com/custom": {} },
                                "f:annotations": { ".": {}, "f:example.com/note": {} },
                            }
                        }))),
                        fields_type: Some("FieldsV1".to_string()),
                        ..Default::default()
                    },
                    // unrelated manager — must not appear in either result
                    ManagedFieldsEntry {
                        manager: Some("other".to_string()),
                        operation: Some("Apply".to_string()),
                        subresource: None,
                        fields_v1: Some(FieldsV1(json!({
                            "f:metadata": { "f:labels": { "f:other.io/x": {} } }
                        }))),
                        fields_type: Some("FieldsV1".to_string()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            ..Default::default()
        };

        let main_keys = managed_metadata_keys(&meta, "operator");
        let tmpl_keys = managed_metadata_keys(&meta, "template.operator");

        assert!(main_keys.labels.contains("app.kubernetes.io/name"));
        assert!(main_keys.annotations.is_empty());
        assert!(
            !main_keys.labels.contains("example.com/custom"),
            "template key must not appear in main_keys"
        );

        assert!(tmpl_keys.labels.contains("example.com/custom"));
        assert!(tmpl_keys.annotations.contains("example.com/note"));
        assert!(
            !tmpl_keys.labels.contains("app.kubernetes.io/name"),
            "main key must not appear in tmpl_keys"
        );
        assert!(
            !tmpl_keys.labels.contains("other.io/x"),
            "unrelated manager key must not appear"
        );
    }

    #[test]
    fn test_filter_owned_removes_conflicting_keys() {
        use crate::crd::MetadataTemplate;

        let owned = ManagedMetadataKeys {
            labels: ["app.kubernetes.io/name".to_string()].into(),
            annotations: ["kaniop.rs/managed".to_string()].into(),
        };
        let tmpl = MetadataTemplate {
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/name".to_string(),
                    "user-value".to_string(),
                ),
                ("example.com/custom".to_string(), "ok".to_string()),
            ])),
            annotations: Some(BTreeMap::from([
                ("kaniop.rs/managed".to_string(), "ignored".to_string()),
                ("example.com/note".to_string(), "kept".to_string()),
            ])),
        };

        let filtered = tmpl.filter_owned(&owned);

        assert!(!filtered.labels.contains_key("app.kubernetes.io/name"));
        assert_eq!(filtered.labels["example.com/custom"], "ok");
        assert!(!filtered.annotations.contains_key("kaniop.rs/managed"));
        assert_eq!(filtered.annotations["example.com/note"], "kept");

        assert_eq!(
            filtered.discarded_labels,
            BTreeSet::from(["app.kubernetes.io/name".to_string()])
        );
        assert_eq!(
            filtered.discarded_annotations,
            BTreeSet::from(["kaniop.rs/managed".to_string()])
        );
        assert!(filtered.has_discards());
    }

    #[test]
    fn test_filter_owned_no_conflicts() {
        use crate::crd::MetadataTemplate;

        let owned = ManagedMetadataKeys {
            labels: ["app.kubernetes.io/name".to_string()].into(),
            annotations: BTreeSet::new(),
        };
        let tmpl = MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/custom".to_string(),
                "val".to_string(),
            )])),
            annotations: None,
        };

        let filtered = tmpl.filter_owned(&owned);

        assert_eq!(filtered.labels["example.com/custom"], "val");
        assert!(filtered.annotations.is_empty());
        assert!(!filtered.has_discards());
    }
}

#[cfg(test)]
mod needs_meta_template_apply_tests {
    use super::*;
    use crate::crd::MetadataTemplate;

    use k8s_openapi::NamespaceResourceScope;
    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
        FieldsV1, ManagedFieldsEntry, ObjectMeta,
    };
    use kube::Resource;
    use kube::api::PartialObjectMeta;
    use serde_json::json;
    use std::borrow::Cow;

    /// Minimal parent resource for testing [`ObjectMetaTemplateExt::needs_meta_template_apply`]
    /// without pulling in a full CRD type.
    #[derive(Clone, Default)]
    struct TestOwner {
        metadata: ObjectMeta,
        template: Option<MetadataTemplate>,
    }

    impl Resource for TestOwner {
        type DynamicType = ();
        type Scope = NamespaceResourceScope;

        fn kind(_: &()) -> Cow<'_, str> {
            "TestOwner".into()
        }
        fn group(_: &()) -> Cow<'_, str> {
            "test.rs".into()
        }
        fn version(_: &()) -> Cow<'_, str> {
            "v1".into()
        }
        fn plural(_: &()) -> Cow<'_, str> {
            "testowners".into()
        }
        fn meta(&self) -> &ObjectMeta {
            &self.metadata
        }
        fn meta_mut(&mut self) -> &mut ObjectMeta {
            &mut self.metadata
        }
    }

    impl ObjectMetaTemplateExt<Secret> for TestOwner {
        const OPERATOR_NAME: &'static str = "test-operator";
        fn managed_object_name(&self) -> String {
            "test-secret".to_string()
        }
        fn metadata_template(&self) -> Option<&MetadataTemplate> {
            self.template.as_ref()
        }
    }

    fn owner(template: Option<MetadataTemplate>) -> TestOwner {
        TestOwner {
            metadata: ObjectMeta {
                name: Some("my-resource".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            template,
        }
    }

    /// Build a `PartialObjectMeta<Secret>` with both actual metadata and managed-field entries
    /// for the template manager (`template.test-operator`).
    fn secret_meta(
        labels: BTreeMap<String, String>,
        annotations: BTreeMap<String, String>,
        template_label_keys: &[&str],
        template_annotation_keys: &[&str],
    ) -> PartialObjectMeta<Secret> {
        let mut managed_fields = vec![];

        if !template_label_keys.is_empty() || !template_annotation_keys.is_empty() {
            let mut meta_fields = serde_json::Map::new();

            if !template_label_keys.is_empty() {
                let mut lf = serde_json::Map::new();
                lf.insert(".".to_string(), json!({}));
                for k in template_label_keys {
                    lf.insert(format!("f:{k}"), json!({}));
                }
                meta_fields.insert("f:labels".to_string(), json!(lf));
            }

            if !template_annotation_keys.is_empty() {
                let mut af = serde_json::Map::new();
                af.insert(".".to_string(), json!({}));
                for k in template_annotation_keys {
                    af.insert(format!("f:{k}"), json!({}));
                }
                meta_fields.insert("f:annotations".to_string(), json!(af));
            }

            managed_fields.push(ManagedFieldsEntry {
                manager: Some("template.test-operator".to_string()),
                operation: Some("Apply".to_string()),
                subresource: None,
                fields_v1: Some(FieldsV1(json!({ "f:metadata": meta_fields }))),
                fields_type: Some("FieldsV1".to_string()),
                ..Default::default()
            });
        }

        PartialObjectMeta {
            metadata: ObjectMeta {
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                annotations: if annotations.is_empty() {
                    None
                } else {
                    Some(annotations)
                },
                managed_fields: if managed_fields.is_empty() {
                    None
                } else {
                    Some(managed_fields)
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_in_sync_no_template_no_stale_fields() {
        let owner = owner(None);
        let live: PartialObjectMeta<Secret> = PartialObjectMeta::default();
        assert!(owner.needs_meta_template_apply(&live).is_none());
    }

    #[test]
    fn test_out_of_sync_no_template_stale_fields_remain() {
        // Template was removed but the template manager still owns fields → cleanup apply needed.
        let owner = owner(None);
        let live = secret_meta(
            BTreeMap::new(),
            BTreeMap::new(),
            &["example.com/stale"],
            &[],
        );
        assert!(owner.needs_meta_template_apply(&live).is_some());
    }

    #[test]
    fn test_in_sync_template_all_keys_present_and_owned() {
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/env".to_string(),
                "prod".to_string(),
            )])),
            annotations: None,
        }));
        let live = secret_meta(
            BTreeMap::from([("example.com/env".to_string(), "prod".to_string())]),
            BTreeMap::new(),
            &["example.com/env"],
            &[],
        );
        assert!(owner.needs_meta_template_apply(&live).is_none());
    }

    #[test]
    fn test_out_of_sync_value_mismatch() {
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/env".to_string(),
                "prod".to_string(),
            )])),
            annotations: None,
        }));
        // Key is present but value differs.
        let live = secret_meta(
            BTreeMap::from([("example.com/env".to_string(), "staging".to_string())]),
            BTreeMap::new(),
            &["example.com/env"],
            &[],
        );
        assert!(owner.needs_meta_template_apply(&live).is_some());
    }

    #[test]
    fn test_out_of_sync_template_manager_owns_stale_extra_key() {
        // Template shrank: manager still owns a key that is no longer desired.
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/env".to_string(),
                "prod".to_string(),
            )])),
            annotations: None,
        }));
        let live = secret_meta(
            BTreeMap::from([
                ("example.com/env".to_string(), "prod".to_string()),
                ("example.com/stale".to_string(), "old".to_string()),
            ]),
            BTreeMap::new(),
            &["example.com/env", "example.com/stale"],
            &[],
        );
        assert!(owner.needs_meta_template_apply(&live).is_some());
    }

    #[test]
    fn test_out_of_sync_template_manager_missing_ownership() {
        // Label present with correct value but template manager doesn't own it yet.
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/env".to_string(),
                "prod".to_string(),
            )])),
            annotations: None,
        }));
        let live = secret_meta(
            BTreeMap::from([("example.com/env".to_string(), "prod".to_string())]),
            BTreeMap::new(),
            &[], // no template manager managed fields
            &[],
        );
        assert!(owner.needs_meta_template_apply(&live).is_some());
    }

    #[test]
    fn test_in_sync_labels_and_annotations() {
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "example.com/env".to_string(),
                "prod".to_string(),
            )])),
            annotations: Some(BTreeMap::from([(
                "example.com/team".to_string(),
                "platform".to_string(),
            )])),
        }));
        let live = secret_meta(
            BTreeMap::from([("example.com/env".to_string(), "prod".to_string())]),
            BTreeMap::from([("example.com/team".to_string(), "platform".to_string())]),
            &["example.com/env"],
            &["example.com/team"],
        );
        assert!(owner.needs_meta_template_apply(&live).is_none());
    }

    #[test]
    fn test_in_sync_all_template_keys_conflict_with_operator() {
        // When every key in the template is already owned by the main operator, filter_owned
        // discards them all. The filtered result is empty and the template manager owns nothing,
        // so no apply is needed — the conflicting keys are silently dropped.
        let owner = owner(Some(MetadataTemplate {
            labels: Some(BTreeMap::from([(
                "app.kubernetes.io/name".to_string(),
                "user-value".to_string(),
            )])),
            annotations: None,
        }));
        // Main operator owns "app.kubernetes.io/name"; template manager owns nothing.
        let live: PartialObjectMeta<Secret> = PartialObjectMeta {
            metadata: ObjectMeta {
                labels: Some(BTreeMap::from([(
                    "app.kubernetes.io/name".to_string(),
                    "operator-value".to_string(),
                )])),
                managed_fields: Some(vec![ManagedFieldsEntry {
                    manager: Some("test-operator".to_string()),
                    operation: Some("Apply".to_string()),
                    subresource: None,
                    fields_v1: Some(FieldsV1(json!({
                        "f:metadata": {
                            "f:labels": { ".": {}, "f:app.kubernetes.io/name": {} }
                        }
                    }))),
                    fields_type: Some("FieldsV1".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(owner.needs_meta_template_apply(&live).is_none());
    }
}
