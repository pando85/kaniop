use kaniop_operator::crd::KanidmRef;

use kube::runtime::reflector::Store;
use kube::{Resource, ResourceExt};

/// Normalize KanidmRef by filling in missing namespace with object's namespace
pub fn normalize_kanidm_ref(kanidm_ref: &KanidmRef, object_namespace: &str) -> (String, String) {
    let ref_name = kanidm_ref.name.clone();
    let ref_namespace = kanidm_ref
        .namespace
        .clone()
        .unwrap_or_else(|| object_namespace.to_string());
    (ref_name, ref_namespace)
}

/// Generic duplicate checker for any resource type
/// Checks for duplicates based on kanidmRef AND the effective Kanidm entity name (kanidmName or K8s name)
pub fn check_duplicate<T>(object: &T, object_name: &str, store: &Store<T>) -> Result<(), String>
where
    T: Resource + ResourceExt + Clone + HasKanidmRef,
    <T as Resource>::DynamicType: Eq + std::hash::Hash + Clone,
{
    let obj_namespace = object.namespace().unwrap_or_else(|| "default".to_string());
    let (ref_name, ref_namespace) = normalize_kanidm_ref(object.kanidm_ref_spec(), &obj_namespace);
    let obj_kanidm_entity_name = object.kanidm_entity_name();

    store
        .state()
        .into_iter()
        .find(|resource| {
            resource.meta().uid != object.meta().uid && {
                let res_namespace = resource
                    .namespace()
                    .unwrap_or_else(|| "default".to_string());
                let (res_ref_name, res_ref_namespace) =
                    normalize_kanidm_ref(resource.kanidm_ref_spec(), &res_namespace);
                ref_name == res_ref_name
                    && ref_namespace == res_ref_namespace
                    && resource.kanidm_entity_name() == obj_kanidm_entity_name
            }
        })
        .map(|r| {
            Err(format!(
                "{} with same kanidmRef and kanidmName already exists: {}/{}",
                object_name,
                r.namespace().unwrap_or_else(|| "default".to_string()),
                r.name_any()
            ))
        })
        .unwrap_or(Ok(()))
}

/// Trait to abstract access to kanidm_ref and kanidm_entity_name across different resource types
pub trait HasKanidmRef {
    fn kanidm_ref_spec(&self) -> &KanidmRef;
    fn kanidm_entity_name(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaniop_operator::crd::KanidmRef;

    #[test]
    fn test_normalize_kanidm_ref_with_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: Some("other-ns".to_string()),
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "other-ns");
    }

    #[test]
    fn test_normalize_kanidm_ref_without_namespace() {
        let ref_obj = KanidmRef {
            name: "my-kanidm".to_string(),
            namespace: None,
        };
        let (name, ns) = normalize_kanidm_ref(&ref_obj, "current-ns");
        assert_eq!(name, "my-kanidm");
        assert_eq!(ns, "current-ns");
    }
}
